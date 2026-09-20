# Caribou installer for Windows — https://github.com/rayzor-blade/caribou
#
#   irm https://caribou.rayzor.tech/install.ps1 | iex
#
# Downloads the nightly `caribou` command, verifies its SHA-256, unpacks it
# into ~/.caribou/bin (the command, ash's runtime image and the DLLs beside
# it, the haxelib and the docs) and adds that directory to your User PATH.
#
#   CARIBOU_INSTALL_DIR   where to install; default ~/.caribou/bin

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest is far faster without the progress bar

if (-not [Environment]::Is64BitOperatingSystem) {
    Write-Error "error: unsupported platform (caribou needs a 64-bit Windows)"
    exit 1
}

$Repo = "rayzor-blade/caribou"
$Dest = if ($env:CARIBOU_INSTALL_DIR) { $env:CARIBOU_INSTALL_DIR } else { Join-Path $HOME ".caribou\bin" }
$Name = "caribou-nightly-x86_64-pc-windows-msvc"
$Asset = "$Name.zip"
$Url = "https://github.com/$Repo/releases/download/nightly/$Asset"

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $TmpDir | Out-Null
$TmpFile = Join-Path $TmpDir $Asset

try {
    Write-Host "downloading $Asset ..."
    try {
        Invoke-WebRequest -Uri $Url -OutFile $TmpFile -ErrorAction Stop
        Invoke-WebRequest -Uri "$Url.sha256" -OutFile "$TmpFile.sha256" -ErrorAction Stop
    } catch {
        Write-Error "error: no nightly archive for Windows at $Url (the last night's build may have failed on this platform; see https://github.com/$Repo/releases/tag/nightly)"
        exit 1
    }

    # The checksum file names the archive as it was written; only the
    # hash is compared.
    $Expected = ((Get-Content "$TmpFile.sha256" -Raw) -split '\s+')[0].ToLower()
    $Actual = (Get-FileHash -Algorithm SHA256 $TmpFile).Hash.ToLower()
    if ($Actual -ne $Expected) {
        Write-Error "error: checksum mismatch for $Asset (got $Actual, expected $Expected)"
        exit 1
    }

    # The archive holds one directory; its contents go into Dest as they
    # are, so the runtime image and the DLLs stay beside the command.
    Expand-Archive -Path $TmpFile -DestinationPath $TmpDir -Force
    if (-not (Test-Path $Dest)) {
        New-Item -ItemType Directory -Path $Dest -Force | Out-Null
    }
    Copy-Item -Path (Join-Path $TmpDir "$Name\*") -Destination $Dest -Recurse -Force

    $Exe = Join-Path $Dest "caribou.exe"

    # A quick sanity run; a missing DLL shows up here, not later.
    $usage = (& $Exe 2>&1 | Out-String)
    if ($usage -notmatch 'usage:') {
        Write-Host "warning: $Exe did not run cleanly. Missing DLLs?" -ForegroundColor Yellow
    }

    $UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    $PathArray = if ($UserPath) { $UserPath -split ';' } else { @() }
    $IsOnPath = $false
    foreach ($p in $PathArray) {
        if ($p.TrimEnd('\') -eq $Dest.TrimEnd('\')) {
            $IsOnPath = $true
            break
        }
    }

    Write-Host ""
    Write-Host "installed: $Exe"
    Write-Host "haxelib:   haxelib dev caribou $(Join-Path $Dest 'haxe')"

    if (-not $IsOnPath) {
        $NewUserPath = if ($UserPath) { "$UserPath;$Dest" } else { $Dest }
        [Environment]::SetEnvironmentVariable("PATH", $NewUserPath, "User")
        $env:PATH = "$env:PATH;$Dest"

        Write-Host ""
        Write-Host "----------------------------------------------------------------"
        Write-Host "caribou has been added to your PATH."
        Write-Host "It is available in this PowerShell session immediately."
        Write-Host "Other terminals (CMD, open VS Code windows) pick it up after a restart."
        Write-Host ""
        Write-Host "Try:  caribou run bin\main.hl"
        Write-Host "----------------------------------------------------------------"
    } else {
        Write-Host ""
        Write-Host "caribou is already on your PATH. Try:  caribou run bin\main.hl"
    }
} finally {
    if (Test-Path $TmpDir) {
        Remove-Item -Recurse -Force $TmpDir
    }
}
