# Install script for ssh-files on Windows
# Usage: irm https://raw.githubusercontent.com/kondyukov/ssh-files/main/install.ps1 | iex
#
# Environment variables:
#   SSH_FILES_INSTALL_DIR - Installation directory (default: %LOCALAPPDATA%\Programs\ssh-files)
#   SSH_FILES_REPO        - GitHub repo (default: kondyukov/ssh-files)

$ErrorActionPreference = "Stop"

# Configuration
$Repo = if ($env:SSH_FILES_REPO) { $env:SSH_FILES_REPO } else { "kondyukov/ssh-files" }
$InstallDir = if ($env:SSH_FILES_INSTALL_DIR) { $env:SSH_FILES_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\ssh-files" }
$BinaryName = "ssh-files.exe"

function Get-LatestVersion {
    $response = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
    return $response.tag_name
}

function Install-SshFiles {
    Write-Host "ssh-files installer for Windows" -ForegroundColor Cyan
    Write-Host "================================" -ForegroundColor Cyan
    Write-Host ""

    # Detect architecture (OSArchitecture sees through a 32-bit or
    # emulated PowerShell process; ARM64 gets its native build)
    $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    $arch = switch ($osArch) {
        "Arm64" { "arm64" }
        default { "x64" }
    }
    $platform = "windows-$arch"
    
    Write-Host "Detected platform: $platform"

    # Get latest version
    $version = Get-LatestVersion
    Write-Host "Latest version: $version"
    Write-Host "Install directory: $InstallDir"
    Write-Host ""

    # Download URL
    $url = "https://github.com/$Repo/releases/download/$version/ssh-files-$platform.zip"
    Write-Host "Downloading from: $url"

    # Create temp directory
    $tempDir = Join-Path $env:TEMP "ssh-files-install"
    $tempZip = Join-Path $tempDir "ssh-files.zip"
    
    if (Test-Path $tempDir) {
        Remove-Item $tempDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $tempDir | Out-Null

    # Download
    try {
        Invoke-WebRequest -Uri $url -OutFile $tempZip -UseBasicParsing
    }
    catch {
        Write-Host "Error downloading: $_" -ForegroundColor Red
        exit 1
    }

    # Extract
    Write-Host "Extracting..."
    Expand-Archive -Path $tempZip -DestinationPath $tempDir -Force

    # Install
    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir | Out-Null
    }

    $sourceBinary = Join-Path $tempDir $BinaryName
    $destBinary = Join-Path $InstallDir $BinaryName
    
    Copy-Item $sourceBinary $destBinary -Force

    # Cleanup
    Remove-Item $tempDir -Recurse -Force

    Write-Host ""
    Write-Host "Successfully installed ssh-files to $destBinary" -ForegroundColor Green

    # Check PATH
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -notlike "*$InstallDir*") {
        Write-Host ""
        Write-Host "Warning: $InstallDir is not in your PATH" -ForegroundColor Yellow
        Write-Host ""
        
        $addToPath = Read-Host "Add to PATH? (Y/n)"
        if ($addToPath -ne "n" -and $addToPath -ne "N") {
            [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
            Write-Host "Added to PATH. Please restart your terminal." -ForegroundColor Green
        }
        else {
            Write-Host ""
            Write-Host "To add manually, run:" -ForegroundColor Cyan
            Write-Host "  `$env:Path += `";$InstallDir`"" -ForegroundColor White
            Write-Host ""
            Write-Host "Or permanently add to your PATH in System Settings > Environment Variables"
        }
    }
    else {
        Write-Host ""
        Write-Host "Run 'ssh-files --help' to get started." -ForegroundColor Cyan
    }
}

Install-SshFiles
