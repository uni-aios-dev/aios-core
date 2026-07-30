#!/usr/bin/env bash
set -euo pipefail

AIOS_VERSION="${AIOS_VERSION:-1.0.0}"
AIOS_BIN="${AIOS_BIN:-aios}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
MODEL_DIR="${MODEL_DIR:-$HOME/.aios/models}"
BLOCKS_DIR="${BLOCKS_DIR:-$HOME/.aios/blocks}"
LOGS_DIR="${LOGS_DIR:-$HOME/.aios/logs}"
MODEL_REPO="${MODEL_REPO:-Qwen/Qwen2.5-0.5B-Instruct-GGUF}"
MODEL_FILE="${MODEL_FILE:-qwen2.5-0.5b-instruct-q4_k_m.gguf}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

log_info()  { echo -e "${CYAN}[AIOS]${NC} $1"; }
log_ok()    { echo -e "${GREEN}[  OK]${NC} $1"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_err()   { echo -e "${RED}[FAIL]${NC} $1"; }

check_dependency() {
    if ! command -v "$1" &>/dev/null; then
        log_err "$1 is not installed. Please install it first."
        log_info "Install with: $2"
        exit 1
    fi
    log_ok "$1 found: $($1 --version 2>&1 | head -1)"
}

detect_platform() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "macos" ;;
        *)       log_err "Unsupported platform: $(uname -s)"; exit 1 ;;
    esac
}

ensure_rust() {
    if command -v rustc &>/dev/null; then
        log_ok "Rust toolchain found: $(rustc --version)"
    else
        log_info "Installing Rust toolchain via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source "$HOME/.cargo/env"
        if ! command -v rustc &>/dev/null; then
            log_err "Rust installation failed. Install manually: https://rustup.rs"
            exit 1
        fi
        log_ok "Rust installed: $(rustc --version)"
    fi
}

build_aios() {
    log_info "Building AIOS v${AIOS_VERSION} in release mode..."
    local start
    start=$(date +%s)
    cargo build --release --bin "$AIOS_BIN"
    local elapsed=$(( $(date +%s) - start ))
    log_ok "Build completed in ${elapsed}s"
}

install_binary() {
    local src
    src="target/release/${AIOS_BIN}"
    if [ ! -f "$src" ]; then
        log_err "Binary not found at ${src}. Build may have failed."
        exit 1
    fi

    if [ ! -w "$INSTALL_DIR" ]; then
        log_info "No write permission to ${INSTALL_DIR}. Installing to ~/.local/bin/"
        INSTALL_DIR="$HOME/.local/bin"
        mkdir -p "$INSTALL_DIR"
    fi

    cp "$src" "$INSTALL_DIR/$AIOS_BIN"
    chmod +x "$INSTALL_DIR/$AIOS_BIN"
    log_ok "Installed ${AIOS_BIN} to ${INSTALL_DIR}"

    if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
        log_warn "${INSTALL_DIR} is not in your PATH."
        log_info "Add this to your ~/.bashrc or ~/.zshrc:"
        echo "export PATH=\"\$PATH:${INSTALL_DIR}\""
    fi
}

setup_directories() {
    mkdir -p "$MODEL_DIR" "$BLOCKS_DIR" "$LOGS_DIR"
    log_ok "Created directories:"
    log_info "  Models:  $MODEL_DIR"
    log_info "  Blocks:  $BLOCKS_DIR"
    log_info "  Logs:    $LOGS_DIR"
}

download_model() {
    if [ -f "${MODEL_DIR}/${MODEL_FILE}" ]; then
        log_ok "Model already exists at ${MODEL_DIR}/${MODEL_FILE}"
        return 0
    fi

    log_info "Downloading default model ${MODEL_REPO}..."
    log_info "  File: ${MODEL_FILE}"
    log_info "  This may take a while depending on your connection."

    if command -v huggingface-cli &>/dev/null; then
        log_info "Using huggingface-cli..."
        huggingface-cli download "$MODEL_REPO" "$MODEL_FILE" --local-dir "$MODEL_DIR"
    elif command -v curl &>/dev/null; then
        local url="https://huggingface.co/${MODEL_REPO}/resolve/main/${MODEL_FILE}"
        log_info "Downloading from $url"
        curl -L -o "${MODEL_DIR}/${MODEL_FILE}" "$url"
    elif command -v wget &>/dev/null; then
        local url="https://huggingface.co/${MODEL_REPO}/resolve/main/${MODEL_FILE}"
        log_info "Downloading from $url"
        wget -O "${MODEL_DIR}/${MODEL_FILE}" "$url"
    else
        log_warn "Cannot download model (no huggingface-cli, curl, or wget)."
        log_info "Download manually: https://huggingface.co/${MODEL_REPO}"
        return 1
    fi

    if [ -f "${MODEL_DIR}/${MODEL_FILE}" ]; then
        log_ok "Model downloaded: ${MODEL_DIR}/${MODEL_FILE}"
    else
        log_warn "Model download may have failed. Check manually: https://huggingface.co/${MODEL_REPO}"
    fi
}

verify_installation() {
    if ! command -v "$AIOS_BIN" &>/dev/null; then
        log_warn "${AIOS_BIN} not found in PATH. Trying ${INSTALL_DIR}/${AIOS_BIN}..."
        if [ -f "${INSTALL_DIR}/${AIOS_BIN}" ]; then
            local ver
            ver=$("${INSTALL_DIR}/${AIOS_BIN}" --version 2>&1 || true)
            log_ok "AIOS binary exists at ${INSTALL_DIR}/${AIOS_BIN}"
            log_info "Version: ${ver:-unknown}"
        else
            log_err "Installation verification failed."
            exit 1
        fi
    else
        local ver
        ver=$("$AIOS_BIN" --version 2>&1 || true)
        log_ok "AIOS v${AIOS_VERSION} installed successfully!"
        log_info "Version: ${ver:-unknown}"
        log_info "Run 'aios' to start the interactive TUI."
        log_info "Run 'aios --daemon' for headless server mode."
    fi
}

main() {
    echo ""
    echo -e "${CYAN}╔══════════════════════════════════════╗${NC}"
    echo -e "${CYAN}║       AIOS v${AIOS_VERSION} Installer       ║${NC}"
    echo -e "${CYAN}╚══════════════════════════════════════╝${NC}"
    echo ""

    local platform
    platform=$(detect_platform)
    log_info "Platform: ${platform}"

    log_info "Checking system dependencies..."
    check_dependency "git"   "apt install git / brew install git"
    check_dependency "curl"  "apt install curl / brew install curl"
    check_dependency "cargo" "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"

    build_aios
    install_binary
    setup_directories
    download_model || true
    verify_installation

    echo ""
    log_ok "AIOS v${AIOS_VERSION} installation complete!"
    echo ""
    echo -e "  ${CYAN}aios${NC}           — Interactive TUI dashboard"
    echo -e "  ${CYAN}aios --daemon${NC}  — Headless server mode"
    echo -e "  ${CYAN}aios --help${NC}    — Show all options"
    echo ""
}

main "$@"
