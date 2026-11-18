//! Secure Boot key material and Authenticode signing for AIOS.
//!
//! `aios-sbctl` builds the PK/KEK/db hierarchy with the host `openssl` CLI,
//! signs a PE/COFF EFI application with an Authenticode PKCS#7 signature and
//! weaves that signature into the image's certificate table. The crate is
//! `std`-only and free of external Rust dependencies so it can be linked into
//! `aios-image` and invoked as a standalone tool.
//!
//! The signing key is the db key: its X.509 certificate is what firmware
//! matches against the enrolled `db` signature database. PK authorizes KEK
//! updates and KEK authorizes db/dbx updates; all three are self-signed here
//! and form the minimal trust chain AIOS controls for its own images.
//!
//! The PKCS#7 layout matches edk2's `AuthenticodeVerify`: `eContentType` is
//! `SPC_INDIRECT_DATA_OBJID` (1.3.6.1.4.1.311.2.1.4), the signature covers
//! `SpcIndirectDataContent` without its outer SEQUENCE header, and `eContent`
//! holds the complete DER value. This is the same dual-content convention
//! used by `sbsign` and accepted by OVMF.

pub mod sha256;

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// RSA key size used for the generated hierarchy.
pub const KEY_BITS: u32 = 2048;
/// Certificate validity period in days.
pub const CERT_DAYS: u32 = 3650;
/// SPC_INDIRECT_DATA_OBJID: Authenticode signed-data content type.
pub const SPC_INDIRECT_DATA_OID: &str = "1.3.6.1.4.1.311.2.1.4";
/// SPC_PE_IMAGE_DATA_OBJID: PE image data inside SpcIndirectDataContent.
const SPC_PE_IMAGE_DATA_OID: &[u32] = &[1, 3, 6, 1, 4, 1, 311, 2, 1, 15];
/// EFI_GLOBAL_VARIABLE GUID in the on-wire mixed-endian form.
const EFI_GLOBAL_VARIABLE: [u8; 16] = [
    0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c,
];
/// EFI_IMAGE_SECURITY_DATABASE GUID in the on-wire mixed-endian form.
const EFI_IMAGE_SECURITY_DATABASE: [u8; 16] = [
    0xcb, 0xb2, 0x19, 0xd7, 0x3a, 0x3d, 0x96, 0x45, 0xa3, 0xbc, 0xda, 0xd0, 0x0e, 0x67, 0x65, 0x6f,
];
/// EFI_CERT_TYPE_PKCS7_GUID in the on-wire mixed-endian form.
const EFI_CERT_TYPE_PKCS7: [u8; 16] = [
    0x9d, 0xd2, 0xaf, 0x4a, 0xdf, 0x68, 0xee, 0x49, 0x8a, 0xa9, 0x34, 0x7d, 0x37, 0x56, 0x65, 0xa7,
];
/// EFI_CERT_X509_GUID in the on-wire mixed-endian form.
const EFI_CERT_X509: [u8; 16] = [
    0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b, 0xf0, 0x72,
];
/// NV | BS | RT | TIME_BASED_AUTHENTICATED_WRITE_ACCESS.
const AUTH_VARIABLE_ATTRS: u32 = 0x27;
/// Fixed GMT timestamp used for all generated enrollment payloads.
const AUTH_TIMESTAMP: [u8; 16] = [0xea, 0x07, 11, 18, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// A generated Secure Boot hierarchy ready for signing and enrollment.
#[derive(Clone, Debug)]
pub struct KeyMaterial {
    /// Root directory that holds the key files.
    pub dir: PathBuf,
    /// Platform Key private key (PEM).
    pub pk_key: PathBuf,
    /// Platform Key certificate (PEM).
    pub pk_crt: PathBuf,
    /// Platform Key certificate (DER) used for enrollment.
    pub pk_cer: PathBuf,
    /// Key Exchange Key private key (PEM).
    pub kek_key: PathBuf,
    /// Key Exchange Key certificate (PEM).
    pub kek_crt: PathBuf,
    /// Key Exchange Key certificate (DER) used for enrollment.
    pub kek_cer: PathBuf,
    /// Signature-database key used to sign EFI binaries (PEM).
    pub db_key: PathBuf,
    /// Signature-database certificate embedded in Authenticode signatures (PEM).
    pub db_crt: PathBuf,
    /// Signature-database certificate (DER) used for enrollment.
    pub db_cer: PathBuf,
}

impl KeyMaterial {
    /// Returns the conventional paths for a hierarchy rooted at `dir`.
    pub fn paths(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            pk_key: dir.join("PK.key"),
            pk_crt: dir.join("PK.crt"),
            pk_cer: dir.join("PK.cer"),
            kek_key: dir.join("KEK.key"),
            kek_crt: dir.join("KEK.crt"),
            kek_cer: dir.join("KEK.cer"),
            db_key: dir.join("DB.key"),
            db_crt: dir.join("DB.crt"),
            db_cer: dir.join("DB.cer"),
        }
    }

    /// Returns `true` when the material needed to sign an image exists.
    pub fn is_complete(&self) -> bool {
        self.db_key.is_file() && self.db_crt.is_file()
    }
}

/// An Authenticode signature and the PE hash it covers.
#[derive(Clone, Debug)]
pub struct Signature {
    /// PKCS#7 SignedData in DER form (WIN_CERTIFICATE content).
    pub pkcs7: Vec<u8>,
    /// SHA-256 PE/COFF image hash (the Authenticode digest).
    pub pe_hash: [u8; 32],
}

/// PE/COFF or ASN.1 parsing error specific to Authenticode signing.
#[derive(Debug)]
pub enum PeError {
    /// The buffer is too small to contain the requested structure.
    Truncated(&'static str),
    /// The DOS or PE signature is missing.
    BadMagic(&'static str),
    /// The image is not a PE32+ (x86-64) executable.
    NotPe32Plus,
    /// The optional header does not have a certificate-table data directory.
    NoCertificateDirectory,
    /// The certificate table points outside the image.
    BadCertificateRange,
    /// A DER structure is malformed or has an unexpected shape.
    InvalidAsn1(&'static str),
}

impl fmt::Display for PeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(what) => write!(f, "truncated PE image: {what}"),
            Self::BadMagic(what) => write!(f, "invalid PE image: {what}"),
            Self::NotPe32Plus => write!(f, "only PE32+ (x86-64) EFI images are supported"),
            Self::NoCertificateDirectory => {
                write!(f, "optional header has no certificate-table directory")
            }
            Self::BadCertificateRange => write!(f, "certificate table lies outside the image"),
            Self::InvalidAsn1(what) => write!(f, "invalid ASN.1 structure: {what}"),
        }
    }
}

impl Error for PeError {}

/// Failure while invoking the external `openssl` tool.
#[derive(Debug)]
pub struct ToolError {
    /// Program that failed.
    pub program: String,
    /// Arguments passed to the program.
    pub args: Vec<String>,
    /// Captured stderr/stdout, trimmed.
    pub output: String,
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {:?} failed: {}",
            self.program, self.args, self.output
        )
    }
}

impl Error for ToolError {}

/// High-level signing/key-management error.
#[derive(Debug)]
pub enum SbError {
    /// PE or ASN.1 structure error.
    Pe(PeError),
    /// External tool failure.
    Tool(ToolError),
    /// Filesystem I/O failure.
    Io(std::io::Error),
    /// The key hierarchy is incomplete.
    MissingKeys(PathBuf),
    /// The image already carries a certificate table.
    AlreadySigned,
    /// The image carries no Authenticode signature.
    NotSigned,
}

impl fmt::Display for SbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pe(e) => write!(f, "{e}"),
            Self::Tool(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::MissingKeys(dir) => {
                write!(f, "incomplete Secure Boot keys in {}", dir.display())
            }
            Self::AlreadySigned => write!(f, "PE image already contains a certificate table"),
            Self::NotSigned => write!(f, "PE image has no Authenticode signature"),
        }
    }
}

impl Error for SbError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Pe(e) => Some(e),
            Self::Tool(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<PeError> for SbError {
    fn from(value: PeError) -> Self {
        Self::Pe(value)
    }
}

impl From<ToolError> for SbError {
    fn from(value: ToolError) -> Self {
        Self::Tool(value)
    }
}

impl From<std::io::Error> for SbError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Offsets of PE fields relevant to Authenticode.
#[derive(Clone, Copy, Debug)]
pub struct PeInfo {
    /// File offset of the PE signature ("PE\0\0").
    pub pe_offset: usize,
    /// File offset of the `CheckSum` field in the optional header.
    pub checksum_offset: usize,
    /// File offset of the certificate-table data-directory entry.
    pub cert_dir_offset: usize,
    /// Declared size of all headers, in bytes.
    pub headers_size: usize,
    /// Existing certificate-table RVA (0 if absent).
    pub cert_rva: u32,
    /// Existing certificate-table size (0 if absent).
    pub cert_size: u32,
}

/// Parses enough of a PE32+ image to locate Authenticode-relevant fields.
pub fn parse_pe(image: &[u8]) -> Result<PeInfo, PeError> {
    if image.len() < 0x40 {
        return Err(PeError::Truncated("DOS header"));
    }
    if &image[0..2] != b"MZ" {
        return Err(PeError::BadMagic("missing MZ signature"));
    }
    let pe_offset = le_u32(image, 0x3c)? as usize;
    if image.len() < pe_offset + 4 + 20 {
        return Err(PeError::Truncated("PE header"));
    }
    if &image[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return Err(PeError::BadMagic("missing PE signature"));
    }
    let coff = pe_offset + 4;
    let opt_size = le_u16(image, coff + 16)? as usize;
    let opt = coff + 20;
    if image.len() < opt + opt_size || opt_size < 112 {
        return Err(PeError::Truncated("optional header"));
    }
    if le_u16(image, opt)? != 0x20b {
        return Err(PeError::NotPe32Plus);
    }
    let headers_size = le_u32(image, opt + 60)? as usize;
    let num_dirs = le_u32(image, opt + 108)? as usize;
    if num_dirs <= 4 {
        return Err(PeError::NoCertificateDirectory);
    }
    let cert_dir_offset = opt + 112 + 4 * 8;
    if image.len() < cert_dir_offset + 8 {
        return Err(PeError::Truncated("certificate directory"));
    }
    let cert_rva = le_u32(image, cert_dir_offset)?;
    let cert_size = le_u32(image, cert_dir_offset + 4)?;
    if cert_rva != 0 || cert_size != 0 {
        let start = cert_rva as usize;
        let end = start.saturating_add(cert_size as usize);
        if end > image.len() || start < headers_size {
            return Err(PeError::BadCertificateRange);
        }
    }
    Ok(PeInfo {
        pe_offset,
        checksum_offset: opt + 64,
        cert_dir_offset,
        headers_size,
        cert_rva,
        cert_size,
    })
}

/// Computes the Authenticode SHA-256 hash of an unsigned PE image.
///
/// The hash covers the image in file order but skips the PE checksum field,
/// the certificate-table data-directory entry and any certificate table.
pub fn authenticode_hash(image: &[u8]) -> Result<[u8; 32], PeError> {
    let info = parse_pe(image)?;
    if info.cert_rva != 0 || info.cert_size != 0 {
        return Err(PeError::BadCertificateRange);
    }
    if info.headers_size > image.len() {
        return Err(PeError::Truncated("headers"));
    }

    let mut hasher = sha256::Sha256::new();
    let mut pos = 0usize;
    for (start, len) in [
        (info.checksum_offset, 4usize),
        (info.cert_dir_offset, 8usize),
    ] {
        if start > info.headers_size || start + len > info.headers_size {
            return Err(PeError::Truncated("optional header fields"));
        }
        hasher.update(&image[pos..start]);
        pos = start + len;
    }
    hasher.update(&image[pos..]);
    Ok(hasher.finish())
}

/// Returns a copy of `image` with the certificate table removed and the
/// certificate directory zeroed, i.e. the view hashed by Authenticode.
fn without_certificate_table(image: &[u8], info: &PeInfo) -> Vec<u8> {
    let end = if info.cert_rva == 0 || info.cert_size == 0 {
        image.len()
    } else {
        (info.cert_rva as usize).min(image.len())
    };
    let mut out = image[..end].to_vec();
    let dir = info.cert_dir_offset;
    if out.len() >= dir + 8 {
        out[dir..dir + 8].fill(0);
    }
    out
}

/// Appends a `WIN_CERTIFICATE` with `pkcs7_der` to an unsigned PE image.
pub fn embed_signature(image: &[u8], pkcs7_der: &[u8]) -> Result<Vec<u8>, SbError> {
    let info = parse_pe(image)?;
    if info.cert_rva != 0 || info.cert_size != 0 {
        return Err(SbError::AlreadySigned);
    }

    let mut out = image.to_vec();
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
    let cert_rva = out.len() as u32;

    let content_len = 8 + pkcs7_der.len();
    let padded_len = (content_len + 7) & !7;
    out.extend_from_slice(&(content_len as u32).to_le_bytes());
    out.extend_from_slice(&0x0200u16.to_le_bytes());
    out.extend_from_slice(&0x0002u16.to_le_bytes());
    out.extend_from_slice(pkcs7_der);
    out.resize(cert_rva as usize + padded_len, 0);

    let dir = info.cert_dir_offset;
    out[dir..dir + 4].copy_from_slice(&cert_rva.to_le_bytes());
    out[dir + 4..dir + 8].copy_from_slice(&(padded_len as u32).to_le_bytes());
    Ok(out)
}

/// Extracts the embedded PKCS#7 DER blob from a signed PE image.
pub fn extract_signature(image: &[u8]) -> Result<Option<Vec<u8>>, PeError> {
    let info = parse_pe(image)?;
    if info.cert_rva == 0 || info.cert_size == 0 {
        return Ok(None);
    }
    let start = info.cert_rva as usize;
    let size = info.cert_size as usize;
    if start + size > image.len() || size < 8 {
        return Err(PeError::BadCertificateRange);
    }
    if le_u16(image, start + 4)? != 0x0200 || le_u16(image, start + 6)? != 0x0002 {
        return Ok(None);
    }
    let declared = (le_u32(image, start)? as usize).min(size).saturating_sub(8);
    let raw = &image[start + 8..start + 8 + declared.min(size - 8)];
    let exact = der_value_length(raw)?;
    Ok(Some(raw[..exact].to_vec()))
}

/// Generates a fresh PK/KEK/db hierarchy under `dir`.
pub fn generate_keys(dir: &Path) -> Result<KeyMaterial, SbError> {
    fs::create_dir_all(dir)?;
    let keys = KeyMaterial::paths(dir);
    generate_pair(
        &keys.pk_key,
        &keys.pk_crt,
        "CN=AIOS Platform Key",
        "critical,CA:TRUE",
        "critical,keyCertSign,cRLSign",
        None,
    )?;
    generate_pair(
        &keys.kek_key,
        &keys.kek_crt,
        "CN=AIOS Key Exchange Key",
        "critical,CA:TRUE",
        "critical,keyCertSign,cRLSign",
        None,
    )?;
    generate_pair(
        &keys.db_key,
        &keys.db_crt,
        "CN=AIOS Secure Boot DB",
        "critical,CA:FALSE",
        "critical,digitalSignature",
        Some("codeSigning"),
    )?;
    for (pem, der) in [
        (&keys.pk_crt, &keys.pk_cer),
        (&keys.kek_crt, &keys.kek_cer),
        (&keys.db_crt, &keys.db_cer),
    ] {
        openssl(&[
            "x509",
            "-in",
            path_str(pem),
            "-outform",
            "DER",
            "-out",
            path_str(der),
        ])?;
    }
    Ok(keys)
}

/// Signs `pe_path` in place with the db key from `keys`.
pub fn sign_pe_file(pe_path: &Path, keys: &KeyMaterial) -> Result<Signature, SbError> {
    if !keys.is_complete() {
        return Err(SbError::MissingKeys(keys.dir.clone()));
    }
    let image = fs::read(pe_path)?;
    let signature = sign_pe_bytes(&image, &keys.db_key, &keys.db_crt)?;
    let signed = embed_signature(&image, &signature.pkcs7)?;
    fs::write(pe_path, signed)?;
    Ok(signature)
}

/// Signs `pe_path` and writes the result to `out_path`.
pub fn sign_pe_file_to(
    pe_path: &Path,
    out_path: &Path,
    keys: &KeyMaterial,
) -> Result<Signature, SbError> {
    if !keys.is_complete() {
        return Err(SbError::MissingKeys(keys.dir.clone()));
    }
    let image = fs::read(pe_path)?;
    let signature = sign_pe_bytes(&image, &keys.db_key, &keys.db_crt)?;
    let signed = embed_signature(&image, &signature.pkcs7)?;
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(out_path, signed)?;
    Ok(signature)
}

/// Produces an Authenticode PKCS#7 signature over `image`.
pub fn sign_pe_bytes(image: &[u8], key: &Path, cert: &Path) -> Result<Signature, SbError> {
    let pe_hash = authenticode_hash(image)?;
    let indirect = spc_indirect_data_content(&pe_hash);
    let body = der_body(&indirect)?;
    let tmp = TempDir::new("aios-sbctl")?;
    let body_path = tmp.path().join("body.der");
    let sig_path = tmp.path().join("sig.der");
    fs::write(&body_path, body)?;
    openssl(&[
        "cms",
        "-sign",
        "-binary",
        "-nodetach",
        "-in",
        path_str(&body_path),
        "-signer",
        path_str(cert),
        "-inkey",
        path_str(key),
        "-outform",
        "DER",
        "-out",
        path_str(&sig_path),
        "-md",
        "sha256",
        "-nosmimecap",
        "-econtent_type",
        SPC_INDIRECT_DATA_OID,
    ])?;
    let raw = fs::read(&sig_path)?;
    let pkcs7 = replace_signed_content(&raw, &indirect)?;
    Ok(Signature { pkcs7, pe_hash })
}

/// Verifies the embedded Authenticode signature of `image` against `cert`.
///
/// This is an offline check of the PKCS#7 signature and the recomputed PE
/// hash; it does not consult firmware databases.
pub fn verify_pe_bytes(image: &[u8], cert: &Path) -> Result<Signature, SbError> {
    let info = parse_pe(image)?;
    let pkcs7 = extract_signature(image)?.ok_or(SbError::NotSigned)?;
    let unsigned = without_certificate_table(image, &info);
    let pe_hash = authenticode_hash(&unsigned)?;
    let indirect = spc_indirect_data_content(&pe_hash);
    let body = der_body(&indirect)?;
    check_e_content(&pkcs7, &indirect)?;

    let tmp = TempDir::new("aios-sbctl")?;
    let body_path = tmp.path().join("body.der");
    let sig_path = tmp.path().join("sig.der");
    fs::write(&body_path, body)?;
    fs::write(&sig_path, &pkcs7)?;
    openssl(&[
        "smime",
        "-verify",
        "-binary",
        "-inform",
        "DER",
        "-in",
        path_str(&sig_path),
        "-content",
        path_str(&body_path),
        "-certfile",
        path_str(cert),
        "-CAfile",
        path_str(cert),
        "-nointern",
        "-noverify",
        "-purpose",
        "any",
    ])?;
    Ok(Signature { pkcs7, pe_hash })
}

/// Verifies the embedded signature of `pe_path` against `keys.db_crt`.
pub fn verify_pe_file(pe_path: &Path, keys: &KeyMaterial) -> Result<Signature, SbError> {
    if !keys.db_crt.is_file() {
        return Err(SbError::MissingKeys(keys.dir.clone()));
    }
    let image = fs::read(pe_path)?;
    verify_pe_bytes(&image, &keys.db_crt)
}

/// Creates `PK.auth`, `KEK.auth` and `DB.auth` time-based authenticated
/// variable payloads for the generated hierarchy.
///
/// Each file is the Data buffer for `SetVariable` with
/// `EFI_VARIABLE_TIME_BASED_AUTHENTICATED_WRITE_ACCESS`: an
/// `EFI_VARIABLE_AUTHENTICATION_2` descriptor whose WIN_CERTIFICATE carries a
/// detached PKCS#7 signature, followed by an `EFI_SIGNATURE_LIST` holding the
/// DER certificate. The signature covers the edk2 serialization
/// `(VariableName, VendorGuid, Attributes, TimeStamp, Payload)`. PK is
/// self-signed, KEK is signed by PK and db is signed by KEK.
pub fn create_enrollment_auth(keys: &KeyMaterial, out_dir: &Path) -> Result<Vec<PathBuf>, SbError> {
    if !(keys.pk_cer.is_file() && keys.kek_cer.is_file() && keys.db_cer.is_file()) {
        return Err(SbError::MissingKeys(keys.dir.clone()));
    }
    fs::create_dir_all(out_dir)?;
    let entries = [
        (
            "PK.auth",
            "PK",
            &EFI_GLOBAL_VARIABLE,
            &keys.pk_cer,
            &keys.pk_key,
            &keys.pk_crt,
        ),
        (
            "KEK.auth",
            "KEK",
            &EFI_GLOBAL_VARIABLE,
            &keys.kek_cer,
            &keys.pk_key,
            &keys.pk_crt,
        ),
        (
            "DB.auth",
            "db",
            &EFI_IMAGE_SECURITY_DATABASE,
            &keys.db_cer,
            &keys.kek_key,
            &keys.kek_crt,
        ),
    ];
    let mut written = Vec::new();
    for (file, variable, guid, data, signer_key, signer_crt) in entries {
        let data = fs::read(data)?;
        let data = x509_signature_list(guid, &data);
        let auth = authenticated_variable(variable, guid, &data, signer_key, signer_crt)?;
        let path = out_dir.join(file);
        fs::write(&path, auth)?;
        written.push(path);
    }
    Ok(written)
}

/// Formats a byte slice as lowercase hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- internals ---------------------------------------------------------------

fn le_u16(buf: &[u8], off: usize) -> Result<u16, PeError> {
    let s = buf
        .get(off..off + 2)
        .ok_or(PeError::Truncated("u16 field"))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn le_u32(buf: &[u8], off: usize) -> Result<u32, PeError> {
    let s = buf
        .get(off..off + 4)
        .ok_or(PeError::Truncated("u32 field"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or_default()
}

fn generate_pair(
    key: &Path,
    crt: &Path,
    subject: &str,
    basic_constraints: &str,
    key_usage: &str,
    eku: Option<&str>,
) -> Result<(), SbError> {
    let mut args = vec![
        "req".to_string(),
        "-new".to_string(),
        "-x509".to_string(),
        "-newkey".to_string(),
        format!("rsa:{KEY_BITS}"),
        "-sha256".to_string(),
        "-days".to_string(),
        CERT_DAYS.to_string(),
        "-subj".to_string(),
        format!("/{subject}"),
        "-nodes".to_string(),
        "-addext".to_string(),
        format!("basicConstraints={basic_constraints}"),
        "-addext".to_string(),
        format!("keyUsage={key_usage}"),
        "-keyout".to_string(),
        path_str(key).to_string(),
        "-out".to_string(),
        path_str(crt).to_string(),
    ];
    if let Some(eku) = eku {
        args.push("-addext".to_string());
        args.push(format!("extendedKeyUsage={eku}"));
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    openssl(&refs)?;
    Ok(())
}

fn openssl(args: &[&str]) -> Result<Vec<u8>, SbError> {
    let program = openssl_program();
    let output = Command::new(&program).args(args).output().map_err(|e| {
        SbError::Tool(ToolError {
            program: program.clone(),
            args: args.iter().map(|s| s.to_string()).collect(),
            output: e.to_string(),
        })
    })?;
    if !output.status.success() {
        let mut msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        if msg.is_empty() {
            msg = format!("exit status {}", output.status);
        }
        return Err(SbError::Tool(ToolError {
            program,
            args: args.iter().map(|s| s.to_string()).collect(),
            output: msg,
        }));
    }
    Ok(output.stdout)
}

fn openssl_program() -> String {
    if let Ok(value) = std::env::var("AIOS_OPENSSL") {
        if !value.is_empty() {
            return value;
        }
    }
    #[cfg(windows)]
    {
        for candidate in [
            r"C:\Program Files\Git\usr\bin\openssl.exe",
            r"C:\Program Files\Git\mingw64\bin\openssl.exe",
            r"C:\Program Files\OpenSSL-Win64\bin\openssl.exe",
        ] {
            if Path::new(candidate).is_file() {
                return candidate.to_string();
            }
        }
    }
    "openssl".to_string()
}

// --- minimal DER helpers -----------------------------------------------------

#[derive(Clone, Debug)]
enum DerValue {
    Primitive { tag: u8, content: Vec<u8> },
    Constructed { tag: u8, children: Vec<DerValue> },
}

impl DerValue {
    fn children_mut(&mut self) -> Result<&mut Vec<DerValue>, PeError> {
        match self {
            Self::Constructed { children, .. } => Ok(children),
            Self::Primitive { .. } => Err(PeError::InvalidAsn1("expected constructed value")),
        }
    }

    fn content_mut(&mut self) -> Result<&mut Vec<u8>, PeError> {
        match self {
            Self::Primitive { content, .. } => Ok(content),
            Self::Constructed { .. } => Err(PeError::InvalidAsn1("expected primitive value")),
        }
    }

    fn encoded(&self) -> Vec<u8> {
        match self {
            Self::Primitive { tag, content } => der_tlv(*tag, content),
            Self::Constructed { tag, children } => der_tlv(
                *tag,
                &children
                    .iter()
                    .flat_map(DerValue::encoded)
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

fn der_length_size(first: u8) -> Result<usize, PeError> {
    if first & 0x80 == 0 {
        Ok(1)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            Err(PeError::InvalidAsn1("unsupported DER length"))
        } else {
            Ok(1 + n)
        }
    }
}

fn der_value_length(data: &[u8]) -> Result<usize, PeError> {
    if data.len() < 2 {
        return Err(PeError::InvalidAsn1("truncated DER value"));
    }
    let len_size = der_length_size(data[1])?;
    if data.len() < 1 + len_size {
        return Err(PeError::InvalidAsn1("truncated DER length"));
    }
    let mut len = 0usize;
    if len_size > 1 {
        for byte in &data[2..1 + len_size] {
            len = (len << 8) | usize::from(*byte);
        }
    } else {
        len = usize::from(data[1]);
    }
    1usize
        .checked_add(len_size)
        .and_then(|v| v.checked_add(len))
        .filter(|v| *v <= data.len())
        .ok_or(PeError::InvalidAsn1("DER value exceeds buffer"))
}

fn der_body(value: &[u8]) -> Result<&[u8], PeError> {
    if value.first() != Some(&0x30) {
        return Err(PeError::InvalidAsn1("expected SEQUENCE"));
    }
    let header = der_length_size(*value.get(1).ok_or(PeError::InvalidAsn1("length"))?)? + 1;
    value
        .get(header..)
        .ok_or(PeError::InvalidAsn1("short SEQUENCE"))
}

fn parse_der(data: &[u8]) -> Result<DerValue, PeError> {
    let (value, used) = parse_der_at(data, 0)?;
    if used != data.len() {
        return Err(PeError::InvalidAsn1("trailing DER bytes"));
    }
    Ok(value)
}

fn parse_der_at(data: &[u8], offset: usize) -> Result<(DerValue, usize), PeError> {
    let tag = *data.get(offset).ok_or(PeError::InvalidAsn1("tag"))?;
    let first = *data.get(offset + 1).ok_or(PeError::InvalidAsn1("length"))?;
    let len_size = der_length_size(first)?;
    if data.len() < offset + 1 + len_size {
        return Err(PeError::InvalidAsn1("truncated length"));
    }
    let mut len = 0usize;
    if len_size > 1 {
        for byte in &data[offset + 2..offset + 1 + len_size] {
            len = (len << 8) | usize::from(*byte);
        }
    } else {
        len = usize::from(first);
    }
    let content_start = offset + 1 + len_size;
    let content_end = content_start
        .checked_add(len)
        .ok_or(PeError::InvalidAsn1("length overflow"))?;
    let content = data
        .get(content_start..content_end)
        .ok_or(PeError::InvalidAsn1("value exceeds buffer"))?;
    let consumed = content_end - offset;
    if tag & 0x20 != 0 {
        let mut children = Vec::new();
        let mut pos = 0usize;
        while pos < content.len() {
            let (child, used) = parse_der_at(content, pos)?;
            children.push(child);
            pos += used;
        }
        Ok((DerValue::Constructed { tag, children }, consumed))
    } else {
        Ok((
            DerValue::Primitive {
                tag,
                content: content.to_vec(),
            },
            consumed,
        ))
    }
}

/// Replaces the PKCS#7 `eContent` OCTET STRING with `content`.
///
/// OpenSSL signs the SPC body but stores whatever bytes it was given in
/// `eContent`. Authenticode needs the full DER value there, so the OCTET
/// STRING is rewritten after signing and all enclosing lengths are rebuilt.
fn replace_signed_content(pkcs7: &[u8], content: &[u8]) -> Result<Vec<u8>, PeError> {
    let mut root = parse_der(pkcs7)?;
    {
        let content_info = root.children_mut()?;
        let wrapper = content_info
            .get_mut(1)
            .ok_or(PeError::InvalidAsn1("ContentInfo content"))?;
        let signed_data = wrapper
            .children_mut()?
            .get_mut(0)
            .ok_or(PeError::InvalidAsn1("SignedData"))?;
        let encapsulated = signed_data
            .children_mut()?
            .get_mut(2)
            .ok_or(PeError::InvalidAsn1("encapContentInfo"))?;
        let explicit = encapsulated
            .children_mut()?
            .get_mut(1)
            .ok_or(PeError::InvalidAsn1("eContent"))?;
        let octets = explicit
            .children_mut()?
            .get_mut(0)
            .ok_or(PeError::InvalidAsn1("eContent OCTET STRING"))?;
        if !matches!(octets, DerValue::Primitive { tag: 0x04, .. }) {
            return Err(PeError::InvalidAsn1("eContent is not an OCTET STRING"));
        }
        *octets.content_mut()? = content.to_vec();
    }
    Ok(root.encoded())
}

/// Checks the edk2 requirement: the last `HashSize` bytes of the encapsulated
/// SpcIndirectDataContent SEQUENCE content must be the PE image hash.
fn check_e_content(pkcs7: &[u8], indirect: &[u8]) -> Result<(), PeError> {
    let root = parse_der(pkcs7)?;
    let wrapper = root
        .children()
        .and_then(|v| v.get(1))
        .ok_or(PeError::InvalidAsn1("ContentInfo content"))?;
    let signed_data = wrapper
        .children()
        .and_then(|v| v.first())
        .ok_or(PeError::InvalidAsn1("SignedData"))?;
    let encapsulated = signed_data
        .children()
        .and_then(|v| v.get(2))
        .ok_or(PeError::InvalidAsn1("encapContentInfo"))?;
    let explicit = encapsulated
        .children()
        .and_then(|v| v.get(1))
        .ok_or(PeError::InvalidAsn1("eContent"))?;
    let actual = match explicit.children().and_then(|v| v.first()) {
        Some(DerValue::Primitive { tag: 0x04, content }) => content.as_slice(),
        _ => return Err(PeError::InvalidAsn1("eContent is not an OCTET STRING")),
    };
    if actual != indirect {
        return Err(PeError::InvalidAsn1(
            "eContent is not SpcIndirectDataContent",
        ));
    }
    Ok(())
}

impl DerValue {
    fn children(&self) -> Option<&[DerValue]> {
        match self {
            Self::Constructed { children, .. } => Some(children),
            Self::Primitive { .. } => None,
        }
    }
}

fn der_len(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else {
        let mut bytes = Vec::new();
        let mut value = len;
        while value > 0 {
            bytes.push((value & 0xff) as u8);
            value >>= 8;
        }
        bytes.reverse();
        let mut out = vec![0x80 | bytes.len() as u8];
        out.extend_from_slice(&bytes);
        out
    }
}

fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&der_len(content.len()));
    out.extend_from_slice(content);
    out
}

fn der_sequence(content: &[u8]) -> Vec<u8> {
    der_tlv(0x30, content)
}

fn der_oid(arcs: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    if arcs.len() >= 2 {
        body.push((arcs[0] * 40 + arcs[1]) as u8);
        for &arc in &arcs[2..] {
            let mut enc = Vec::new();
            let mut v = arc;
            enc.push((v & 0x7f) as u8);
            v >>= 7;
            while v > 0 {
                enc.push(((v & 0x7f) as u8) | 0x80);
                v >>= 7;
            }
            enc.reverse();
            body.extend_from_slice(&enc);
        }
    }
    der_tlv(0x06, &body)
}

/// Builds `SpcIndirectDataContent` for `pe_hash` (SHA-256).
///
/// The layout is the one used by `sbsign`: SPC_PE_IMAGE_DATA with the
/// conventional `<<<Obsolete>>>` moniker and a standard SHA-256 DigestInfo.
/// The last 32 bytes of the outer SEQUENCE content are the PE hash, which is
/// what edk2's `AuthenticodeVerify` compares against the computed hash.
pub fn spc_indirect_data_content(pe_hash: &[u8; 32]) -> Vec<u8> {
    let obsolete: Vec<u8> = "<<<Obsolete>>>"
        .encode_utf16()
        .flat_map(|unit| unit.to_be_bytes())
        .collect();
    let bmp = der_tlv(0x80, &obsolete);
    let link = der_tlv(0xa2, &bmp);
    let file = der_tlv(0xa0, &link);
    let flags = der_tlv(0x03, &[0x00]);
    let pe_image_data = der_sequence(&[flags, file].concat());
    let data = der_sequence(&[der_oid(SPC_PE_IMAGE_DATA_OID), pe_image_data].concat());

    let sha256_oid = der_oid(&[2, 16, 840, 1, 101, 3, 4, 2, 1]);
    let alg = der_sequence(&[sha256_oid, vec![0x05, 0x00]].concat());
    let digest = der_sequence(&[alg, der_tlv(0x04, pe_hash)].concat());
    der_sequence(&[data, digest].concat())
}

// --- authenticated variable payloads ----------------------------------------

fn x509_signature_list(owner: &[u8; 16], cert: &[u8]) -> Vec<u8> {
    let signature_size = 16 + cert.len();
    let mut out = Vec::with_capacity(28 + signature_size);
    out.extend_from_slice(&EFI_CERT_X509);
    out.extend_from_slice(&((28 + signature_size) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(signature_size as u32).to_le_bytes());
    out.extend_from_slice(owner);
    out.extend_from_slice(cert);
    out
}

fn auth_signed_content(variable: &str, guid: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for unit in variable.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(guid);
    out.extend_from_slice(&AUTH_VARIABLE_ATTRS.to_le_bytes());
    out.extend_from_slice(&AUTH_TIMESTAMP);
    out.extend_from_slice(data);
    out
}

/// Serialize the `EFI_VARIABLE_AUTHENTICATION_2` descriptor header.
///
/// `WIN_CERTIFICATE.wCertificateType` must be `WIN_CERT_TYPE_EFI_GUID`
/// (0x0ef1) when `CertType` carries `EFI_CERT_TYPE_PKCS7_GUID`. EDK2's
/// `VerifyTimeBasedPayload` rejects any other value with
/// `EFI_SECURITY_VIOLATION`. Note that PE/COFF Authenticode uses 0x0002
/// (`WIN_CERT_TYPE_PKCS_SIGNED_DATA`) instead; that value is only correct for
/// `embed_signature`.
fn authentication_descriptor(signature_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&AUTH_TIMESTAMP);
    out.extend_from_slice(&((24 + signature_len) as u32).to_le_bytes());
    out.extend_from_slice(&0x0200u16.to_le_bytes());
    out.extend_from_slice(&0x0ef1u16.to_le_bytes());
    out.extend_from_slice(&EFI_CERT_TYPE_PKCS7);
    out
}

fn authenticated_variable(
    variable: &str,
    guid: &[u8; 16],
    data: &[u8],
    signer_key: &Path,
    signer_cert: &Path,
) -> Result<Vec<u8>, SbError> {
    let content = auth_signed_content(variable, guid, data);
    let tmp = TempDir::new("aios-sbctl")?;
    let content_path = tmp.path().join("auth-content.bin");
    let sig_path = tmp.path().join("auth-sig.der");
    fs::write(&content_path, &content)?;
    openssl(&[
        "cms",
        "-sign",
        "-binary",
        "-in",
        path_str(&content_path),
        "-signer",
        path_str(signer_cert),
        "-inkey",
        path_str(signer_key),
        "-outform",
        "DER",
        "-out",
        path_str(&sig_path),
        "-md",
        "sha256",
        "-nosmimecap",
    ])?;
    let signature = fs::read(&sig_path)?;

    let mut out = authentication_descriptor(signature.len());
    out.extend_from_slice(&signature);
    out.extend_from_slice(data);
    Ok(out)
}

// --- temp dir helper --------------------------------------------------------

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self, std::io::Error> {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_serialization_matches_edk2_layout() {
        let content = auth_signed_content("KEK", &EFI_GLOBAL_VARIABLE, &[0xaa, 0xbb]);
        let mut expected = b"K\0E\0K\0".to_vec();
        expected.extend_from_slice(&EFI_GLOBAL_VARIABLE);
        expected.extend_from_slice(&0x27u32.to_le_bytes());
        expected.extend_from_slice(&AUTH_TIMESTAMP);
        expected.extend_from_slice(&[0xaa, 0xbb]);
        assert_eq!(content, expected);
        assert_eq!(content.len(), 6 + 16 + 4 + 16 + 2);
    }

    #[test]
    fn x509_signature_list_matches_uefi_layout() {
        let cert = [0x30, 0x82, 0x01, 0x02];
        let list = x509_signature_list(&EFI_GLOBAL_VARIABLE, &cert);
        assert_eq!(list.len(), 28 + 16 + cert.len());
        assert_eq!(&list[0..16], &EFI_CERT_X509);
        assert_eq!(
            u32::from_le_bytes(list[16..20].try_into().unwrap()),
            list.len() as u32
        );
        assert_eq!(u32::from_le_bytes(list[20..24].try_into().unwrap()), 0);
        assert_eq!(
            u32::from_le_bytes(list[24..28].try_into().unwrap()),
            (16 + cert.len()) as u32
        );
        assert_eq!(&list[28..44], &EFI_GLOBAL_VARIABLE);
        assert_eq!(&list[44..], &cert);
    }

    #[test]
    fn auth_descriptor_uses_efi_guid_certificate_type() {
        let header = authentication_descriptor(7);
        assert_eq!(header.len(), 40);
        assert_eq!(
            u32::from_le_bytes(header[16..20].try_into().unwrap()),
            24 + 7
        );
        assert_eq!(
            u16::from_le_bytes(header[20..22].try_into().unwrap()),
            0x0200
        );
        // WIN_CERT_TYPE_EFI_GUID, not the PE-only WIN_CERT_TYPE_PKCS_SIGNED_DATA.
        assert_eq!(
            u16::from_le_bytes(header[22..24].try_into().unwrap()),
            0x0ef1
        );
        assert_eq!(&header[24..40], &EFI_CERT_TYPE_PKCS7);
    }

    #[test]
    fn auth_timestamp_has_valid_efi_time_layout() {
        assert_eq!(AUTH_TIMESTAMP.len(), 16);
        assert_eq!(
            u16::from_le_bytes([AUTH_TIMESTAMP[0], AUTH_TIMESTAMP[1]]),
            2026
        );
        assert_eq!(AUTH_TIMESTAMP[2..8], [11, 18, 12, 0, 0, 0]);
    }

    fn minimal_pe32plus() -> Vec<u8> {
        let mut pe = vec![0u8; 512];
        pe[0..2].copy_from_slice(b"MZ");
        let pe_off = 0x80u32;
        pe[0x3c..0x40].copy_from_slice(&pe_off.to_le_bytes());
        let p = pe_off as usize;
        pe[p..p + 4].copy_from_slice(b"PE\0\0");
        pe[p + 20..p + 22].copy_from_slice(&240u16.to_le_bytes());
        let opt = p + 24;
        pe[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        pe[opt + 60..opt + 64].copy_from_slice(&512u32.to_le_bytes());
        pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        pe
    }

    #[test]
    fn parses_minimal_pe() {
        let pe = minimal_pe32plus();
        let info = parse_pe(&pe).expect("parse");
        assert_eq!(info.pe_offset, 0x80);
        assert_eq!(info.headers_size, 512);
        assert_eq!(info.cert_rva, 0);
        assert_eq!(info.cert_size, 0);
    }

    #[test]
    fn hash_changes_when_body_changes() {
        let mut a = minimal_pe32plus();
        let mut b = a.clone();
        b[200] ^= 0x5a;
        assert_ne!(
            authenticode_hash(&a).expect("hash a"),
            authenticode_hash(&b).expect("hash b")
        );
        a[100] ^= 1;
        assert_ne!(
            authenticode_hash(&a).expect("hash a2"),
            authenticode_hash(&b).expect("hash b2")
        );
    }

    #[test]
    fn hash_ignores_checksum_and_cert_dir() {
        let a = minimal_pe32plus();
        let mut b = a.clone();
        let info = parse_pe(&a).expect("parse");
        b[info.checksum_offset] ^= 0xff;
        b[info.checksum_offset + 3] ^= 0x42;
        assert_eq!(
            authenticode_hash(&a).expect("a"),
            authenticode_hash(&b).expect("b")
        );
    }

    #[test]
    fn embed_and_extract_roundtrip() {
        let pe = minimal_pe32plus();
        let fake_pkcs7 = vec![0x30, 0x03, 0x02, 0x01, 0x00];
        let signed = embed_signature(&pe, &fake_pkcs7).expect("embed");
        let info = parse_pe(&signed).expect("parse signed");
        assert_ne!(info.cert_rva, 0);
        assert_eq!(info.cert_rva as usize % 8, 0);
        assert_eq!(
            extract_signature(&signed)
                .expect("extract")
                .expect("present"),
            fake_pkcs7
        );
    }

    #[test]
    fn extract_trims_certificate_padding() {
        let pe = minimal_pe32plus();
        let fake_pkcs7 = vec![0x30, 0x03, 0x02, 0x01, 0x00];
        let mut signed = embed_signature(&pe, &fake_pkcs7).expect("embed");
        let info = parse_pe(&signed).expect("parse");
        let table = info.cert_rva as usize;
        let dir = info.cert_dir_offset;
        let inflated = (info.cert_size / 8 + 1) * 8;
        signed.resize(table + inflated as usize, 0);
        signed[dir + 4..dir + 8].copy_from_slice(&inflated.to_le_bytes());
        assert_eq!(
            extract_signature(&signed)
                .expect("extract")
                .expect("present"),
            fake_pkcs7
        );
        assert!(table.is_multiple_of(8));
    }

    #[test]
    fn refuses_double_sign() {
        let pe = minimal_pe32plus();
        let signed = embed_signature(&pe, &[0x30, 0x00]).expect("embed");
        assert!(matches!(
            embed_signature(&signed, &[0x30, 0x00]).unwrap_err(),
            SbError::AlreadySigned
        ));
    }

    #[test]
    fn spc_content_has_hash_at_end() {
        let hash = [0x11u8; 32];
        let spc = spc_indirect_data_content(&hash);
        assert_eq!(spc[0], 0x30);
        let body = der_body(&spc).expect("body");
        assert!(body.ends_with(&hash), "hash must be the trailing DER field");
    }

    #[test]
    fn parses_sequence_with_multiple_children() {
        let inner = der_sequence(&der_tlv(0x04, &[0xaa; 5]));
        let value =
            der_sequence(&[der_tlv(0x02, &[1]), inner.clone(), der_tlv(0x02, &[2])].concat());
        let parsed = parse_der(&value).expect("parse");
        let children = parsed.children().expect("children");
        assert_eq!(
            children.len(),
            3,
            "every child of a SEQUENCE must be parsed"
        );
        assert_eq!(children[1].encoded(), inner);
    }

    #[test]
    fn der_helpers_roundtrip_lengths() {
        let payload = vec![0x5a; 300];
        let value = der_tlv(0x04, &payload);
        assert_eq!(der_value_length(&value).expect("len"), value.len());
        assert_eq!(der_body(&der_sequence(&value)).expect("body"), &value);
    }

    #[test]
    fn rejects_unsigned_image() {
        let pe = minimal_pe32plus();
        assert!(extract_signature(&pe).expect("extract").is_none());
    }
}
