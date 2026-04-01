# install.ps1 — download and install the st binary on Windows
# Usage: iwr -useb https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.ps1 | iex
#   or: powershell -ExecutionPolicy Bypass -File install.ps1

$ErrorActionPreference = 'Stop'

$repo    = 'whit3rabbit/syntext'
$binary  = 'st.exe'
$installDir = Join-Path $env:LOCALAPPDATA 'syntext'

# Fetch latest release tag from GitHub API.
$apiUrl  = "https://api.github.com/repos/$repo/releases/latest"
$release = Invoke-RestMethod -Uri $apiUrl -UseBasicParsing
$version = $release.tag_name          # e.g. "v1.0.0"
$ver     = $version.TrimStart('v')    # e.g. "1.0.0"

$assetName = "st-${ver}-windows-amd64.exe"
$asset     = $release.assets | Where-Object { $_.name -eq $assetName }
if (-not $asset) {
    Write-Error "No asset named '$assetName' found in release $version."
    exit 1
}

New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$dest = Join-Path $installDir $binary

Write-Host "Downloading $assetName..."
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $dest -UseBasicParsing
Write-Host "Installed to $dest"

# Add to user PATH if not already present.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$installDir*") {
    [Environment]::SetEnvironmentVariable(
        'Path',
        "$userPath;$installDir",
        'User'
    )
    Write-Host "Added $installDir to user PATH. Restart your terminal to use 'st'."
} else {
    Write-Host "'$installDir' is already in PATH."
}
