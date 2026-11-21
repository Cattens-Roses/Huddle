param(
    [string]$Version = "0.0.0",
    [string]$Executable = "target/release/huddle.exe",
    [string]$Output = "dist"
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "../..")
$source = Resolve-Path (Join-Path $root $Executable)
$outputPath = Join-Path $root $Output
New-Item -ItemType Directory -Force $outputPath | Out-Null

$iscc = @(
    (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6/ISCC.exe")
    (Join-Path $env:LOCALAPPDATA "Programs/Inno Setup 6/ISCC.exe")
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    throw "Inno Setup 6 is not installed"
}

$env:HUDDLE_VERSION = $Version.TrimStart("v")
$env:HUDDLE_EXE = $source.Path
$env:HUDDLE_DIST = (Resolve-Path $outputPath).Path

& $iscc (Join-Path $root "packaging/windows/huddle.iss")
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed with exit code $LASTEXITCODE"
}
