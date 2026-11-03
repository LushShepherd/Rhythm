$ErrorActionPreference = 'Stop'
$rhythmDesktopArguments = $args
$rhythmRoot = Split-Path -Parent $PSScriptRoot
$rhythmLocalCargo = Join-Path $rhythmRoot '.tools\cargo\bin'
if (Test-Path -LiteralPath $rhythmLocalCargo) {
    $env:CARGO_HOME = Join-Path $rhythmRoot '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $rhythmRoot '.tools\rustup'
    $env:PATH = $rhythmLocalCargo + ';' + $env:PATH
}
$rhythmTauri = Join-Path $rhythmRoot 'apps\desktop\node_modules\.bin\tauri.cmd'
if (-not (Test-Path -LiteralPath $rhythmTauri)) {
    throw 'Run npm ci in apps/desktop first.'
}
$rhythmTauriArguments = $rhythmDesktopArguments
if ($rhythmDesktopArguments.Count -gt 0 -and $rhythmDesktopArguments[0] -eq 'build') {
    # Ship the machine-control client beside the desktop executable. Keep generated
    # resource configuration out of ordinary cargo test/dev builds and other platforms.
    & (Join-Path $PSScriptRoot 'cargo.ps1') build --release -p rhythm-cli --locked
    if ($LASTEXITCODE -ne 0) { throw 'The Rhythm CLI build failed.' }
    $rhythmPackageDirectory = Join-Path $rhythmRoot '.tools'
    New-Item -ItemType Directory -Path $rhythmPackageDirectory -Force | Out-Null
    $rhythmPackageConfig = Join-Path $rhythmPackageDirectory 'rhythm-package-config.json'
    $rhythmResourceMap = @{}
    $rhythmResourceMap[(Join-Path $rhythmRoot 'target/release/rhythm-cli.exe')] = 'rhythm-cli.exe'
    @{ bundle = @{ resources = $rhythmResourceMap } } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $rhythmPackageConfig -Encoding utf8
    $rhythmTauriArguments = @('build', '--config', $rhythmPackageConfig) + @($rhythmDesktopArguments | Select-Object -Skip 1)
}
Push-Location (Join-Path $rhythmRoot 'apps\desktop')
try {
    & $rhythmTauri @rhythmTauriArguments
    $rhythmExitCode = $LASTEXITCODE
} finally {
    Pop-Location
}
exit $rhythmExitCode
