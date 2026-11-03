$ErrorActionPreference = 'Stop'
$rhythmCargoArguments = $args
$rhythmRoot = Split-Path -Parent $PSScriptRoot
$rhythmCargo = Join-Path $rhythmRoot '.tools\cargo\bin\cargo.exe'
if (Test-Path -LiteralPath $rhythmCargo) {
    $env:CARGO_HOME = Join-Path $rhythmRoot '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $rhythmRoot '.tools\rustup'
    $env:PATH = (Join-Path $rhythmRoot '.tools\cargo\bin') + ';' + $env:PATH
} else {
    $rhythmCargo = (Get-Command cargo -ErrorAction Stop).Source
}

Push-Location $rhythmRoot
try {
    & $rhythmCargo @rhythmCargoArguments
    $rhythmExitCode = $LASTEXITCODE
} finally {
    Pop-Location
}
exit $rhythmExitCode
