param(
    [string]$InstallDir = "$env:LOCALAPPDATA\aios",
    [string]$ModelDir   = "$env:USERPROFILE\.aios\models",
    [string]$BlocksDir  = "$env:USERPROFILE\.aios\blocks",
    [string]$LogsDir    = "$env:USERPROFILE\.aios\logs",
    [string]$ModelRepo  = "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
    [string]$ModelFile  = "qwen2.5-0.5b-instruct-q4_k_m.gguf"
)

$AiosVersion = "1.0.0"
$AiosBin = "aios.exe"
$BinPath = Join-Path $InstallDir $AiosBin

$Host.UI.RawUI.ForegroundColor = "Cyan"
Write-Host "╔══════════════════════════════════════╗"
Write-Host "║       AIOS v$AiosVersion Installer       ║"
Write-Host "╚══════════════════════════════════════╝"
Write-Host ""
$Host.UI.RawUI.ForegroundColor = "White"

function Write-Info  { Write-Host "[AIOS]" -ForegroundColor Cyan -NoNewline; Write-Host " $args" }
function Write-Ok    { Write-Host "[  OK]" -ForegroundColor Green -NoNewline; Write-Host " $args" }
function Write-Warn  { Write-Host "[WARN]" -ForegroundColor Yellow -NoNewline; Write-Host " $args" }
function Write-Err   { Write-Host "[FAIL]" -ForegroundColor Red -NoNewline; Write-Host " $args" }

function Test-Command {
    param([string]$Command)
    $oldPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Stop'
    try {
        Get-Command $Command -ErrorAction Stop | Out-Null
        return $true
    } catch {
        return $false
    } finally {
        $ErrorActionPreference = $oldPreference
    }
}

function Ensure-Dependency {
    param([string]$Name, [string]$InstallHint)
    if (Test-Command $Name) {
        $ver = & $Name --version 2>&1 | Select-Object -First 1
        Write-Ok "$Name found: $ver"
        return $true
    } else {
        Write-Err "$Name is not installed."
        Write-Info $InstallHint
        return $false
    }
}

function Ensure-Rust {
    if (Test-Command "cargo") {
        $ver = & cargo --version
        Write-Ok "Rust toolchain found: $ver"
        return
    }

    Write-Info "Installing Rust toolchain via rustup..."
    $url = "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe"
    $tmp = [System.IO.Path]::GetTempFileName() + ".exe"
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
        Start-Process -Wait -FilePath $tmp -ArgumentList "-y", "--default-host", "x86_64-pc-windows-msvc"
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "User") + ";" + `
                     [System.Environment]::GetEnvironmentVariable("Path", "Machine")
        if (Test-Command "cargo") {
            Write-Ok "Rust installed: $(cargo --version)"
        } else {
            Write-Err "Rust installation may have failed. Install manually: https://rustup.rs"
            exit 1
        }
    } finally {
        Remove-Item -Force $tmp -ErrorAction SilentlyContinue
    }
}

function Build-Aios {
    Write-Info "Building AIOS v$AiosVersion in release mode..."
    $start = Get-Date
    & cargo build --release --bin aios
    if ($LASTEXITCODE -ne 0) {
        Write-Err "Build failed. Check the output above for errors."
        exit 1
    }
    $elapsed = [math]::Round(((Get-Date) - $start).TotalSeconds, 0)
    Write-Ok "Build completed in ${elapsed}s"
}

function Install-Binary {
    $src = "target\release\$AiosBin"
    if (-not (Test-Path $src)) {
        Write-Err "Binary not found at $src. Build may have failed."
        exit 1
    }

    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    }

    Copy-Item -Path $src -Destination $BinPath -Force
    Write-Ok "Installed $AiosBin to $InstallDir"

    $userPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -notlike "*$InstallDir*") {
        $newPath = "$InstallDir;$userPath"
        [System.Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Ok "Added $InstallDir to user PATH"
        Write-Info "Restart your terminal or run: `$env:Path += `";$InstallDir`""
    }
}

function Setup-Directories {
    $dirs = @($ModelDir, $BlocksDir, $LogsDir)
    foreach ($dir in $dirs) {
        if (-not (Test-Path $dir)) {
            New-Item -ItemType Directory -Path $dir -Force | Out-Null
        }
    }
    Write-Ok "Created directories:"
    Write-Info "  Models:  $ModelDir"
    Write-Info "  Blocks:  $BlocksDir"
    Write-Info "  Logs:    $LogsDir"
}

function Download-Model {
    $modelPath = Join-Path $ModelDir $ModelFile
    if (Test-Path $modelPath) {
        Write-Ok "Model already exists at $modelPath"
        return
    }

    Write-Info "Downloading default model $ModelRepo ..."
    Write-Info "  File: $ModelFile"
    Write-Info "  This may take a while depending on your connection."

    if (Test-Command "huggingface-cli") {
        Write-Info "Using huggingface-cli..."
        & huggingface-cli download $ModelRepo $ModelFile --local-dir $ModelDir
        if ($LASTEXITCODE -eq 0 -and (Test-Path $modelPath)) {
            Write-Ok "Model downloaded: $modelPath"
            return
        }
    }

    $url = "https://huggingface.co/$ModelRepo/resolve/main/$ModelFile"
    Write-Info "Downloading from $url"
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $url -OutFile $modelPath -UseBasicParsing
        if (Test-Path $modelPath) {
            Write-Ok "Model downloaded: $modelPath"
        } else {
            throw "Download failed"
        }
    } catch {
        Write-Warn "Model download failed: $_"
        Write-Info "Download manually from: https://huggingface.co/$ModelRepo"
    }
}

function Verify-Installation {
    if (Test-Command $AiosBin) {
        $ver = & $AiosBin --version 2>&1 | Select-Object -First 1
        Write-Ok "AIOS v$AiosVersion installed successfully!"
        Write-Info "Version: $ver"
    } elseif (Test-Path $BinPath) {
        Write-Ok "AIOS binary exists at $BinPath"
        Write-Warn "Restart your terminal or add $InstallDir to PATH manually."
    } else {
        Write-Err "Installation verification failed."
        exit 1
    }

    Write-Host ""
    Write-Info "Run 'aios' to start the interactive TUI."
    Write-Info "Run 'aios --daemon' for headless server mode."
    Write-Host ""
}

function Main {
    Write-Info "Checking system dependencies..."
    $depsOk = $true
    if (-not (Ensure-Dependency "git" "Install Git for Windows: https://git-scm.com")) { $depsOk = $false }
    if (-not (Ensure-Dependency "cargo" "Install Rust: https://rustup.rs")) { $depsOk = $false }
    if (-not $depsOk) { exit 1 }

    Ensure-Rust
    Build-Aios
    Install-Binary
    Setup-Directories
    Download-Model
    Verify-Installation

    Write-Ok "AIOS v$AiosVersion installation complete!"
}

Main
