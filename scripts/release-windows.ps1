# Builds a release binary and packages it as dist\HopDesk-<version>-windows-x64.zip
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$name = "HopDesk-$version-windows-x64"
$stage = Join-Path ([IO.Path]::GetTempPath()) $name

Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, dist | Out-Null
Copy-Item target\release\hopdesk.exe (Join-Path $stage 'HopDesk.exe')

$zip = "dist\$name.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Remove-Item -Recurse -Force $stage
(Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower() + "  $zip"
