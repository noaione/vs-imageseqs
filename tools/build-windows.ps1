# Build the plugin-only Windows wheel from a Visual Studio developer shell.
# By default this installs the manifest and pkgconf into vcpkg_installed, then
# writes the wheel to dist. VCPKG_INSTALLATION_ROOT can select the vcpkg.exe.
[CmdletBinding()]
param(
    [string] $VcpkgRoot = $env:VCPKG_INSTALLATION_ROOT,
    [string] $InstallRoot,
    [string] $OutputDirectory = "$(Split-Path -Parent $PSScriptRoot)\dist",
    [switch] $SkipVcpkgInstall
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($VcpkgRoot)) {
    $VcpkgRoot = 'C:\vcpkg'
}
if ([string]::IsNullOrWhiteSpace($InstallRoot)) {
    $InstallRoot = Join-Path $repoRoot 'vcpkg_installed'
}

$VcpkgRoot = [System.IO.Path]::GetFullPath($VcpkgRoot)
$InstallRoot = [System.IO.Path]::GetFullPath($InstallRoot)
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$vcpkg = Join-Path $VcpkgRoot 'vcpkg.exe'
$triplet = 'x64-windows-static-md'
$adapter = Join-Path $repoRoot 'target\vcpkg-root'
$adapterInstalled = Join-Path $adapter 'installed'
$pkgconfDirectory = Join-Path $InstallRoot 'x64-windows\tools\pkgconf'
$pkgconf = Join-Path $pkgconfDirectory 'pkgconf.exe'
$pkgconfig = Join-Path $InstallRoot "$triplet\lib\pkgconfig"

if (-not (Test-Path -LiteralPath $vcpkg -PathType Leaf)) {
    throw "vcpkg was not found at '$vcpkg'. Set VCPKG_INSTALLATION_ROOT or pass -VcpkgRoot."
}

Push-Location $repoRoot
try {
    New-Item -ItemType Directory -Force -Path $InstallRoot | Out-Null

    if (-not $SkipVcpkgInstall) {
        & $vcpkg install `
            --triplet $triplet `
            "--x-manifest-root=$repoRoot" `
            "--x-install-root=$InstallRoot"
        if ($LASTEXITCODE -ne 0) {
            throw "vcpkg manifest install failed with exit code $LASTEXITCODE"
        }

        # dav1d-sys discovers dav1d with pkg-config rather than vcpkg-rs.
        & $vcpkg install 'pkgconf:x64-windows' --classic "--x-install-root=$InstallRoot"
        if ($LASTEXITCODE -ne 0) {
            throw "vcpkg pkgconf install failed with exit code $LASTEXITCODE"
        }
    }

    if (-not (Test-Path -LiteralPath $InstallRoot -PathType Container)) {
        throw "vcpkg install root does not exist: '$InstallRoot'"
    }
    if (-not (Test-Path -LiteralPath $pkgconf -PathType Leaf)) {
        throw "pkgconf was not found at '$pkgconf'. Install dependencies or omit -SkipVcpkgInstall."
    }
    if (-not (Test-Path -LiteralPath $pkgconfig -PathType Container)) {
        throw "vcpkg pkg-config files were not found at '$pkgconfig'. Install the manifest dependencies first."
    }

    New-Item -ItemType Directory -Force -Path $adapter | Out-Null
    $marker = Join-Path $adapter '.vcpkg-root'
    if (Test-Path -LiteralPath $marker) {
        if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
            throw "Refusing to replace non-file vcpkg marker '$marker'."
        }
    }
    else {
        New-Item -ItemType File -Path $marker | Out-Null
    }

    if (Test-Path -LiteralPath $adapterInstalled) {
        $existing = Get-Item -LiteralPath $adapterInstalled -Force
        if (($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) {
            throw "Refusing to replace non-junction vcpkg adapter path '$adapterInstalled'."
        }
        Remove-Item -LiteralPath $adapterInstalled -Force
    }
    New-Item -ItemType Junction -Path $adapterInstalled -Target $InstallRoot | Out-Null

    $env:VCPKG_ROOT = $adapter
    $env:VCPKGRS_TRIPLET = $triplet
    $env:PKG_CONFIG = $pkgconf
    $env:PKG_CONFIG_PATH = $pkgconfig
    $env:PATH = "$pkgconfDirectory$([IO.Path]::PathSeparator)$env:PATH"

    python -m pip install --upgrade pip build
    if ($LASTEXITCODE -ne 0) {
        throw "Installing the Python build tools failed with exit code $LASTEXITCODE"
    }

    python tools/build_output.py clear $OutputDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "Clearing the wheel output failed with exit code $LASTEXITCODE"
    }

    python -m build --wheel --outdir $OutputDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "Building the Windows wheel failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}
