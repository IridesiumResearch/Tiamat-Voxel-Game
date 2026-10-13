# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# Signs every executable of a staged Windows release in place, between
# `package.sh --stage-only` and `package.sh --archive-from`, with a
# certificate file (PFX). The release workflow prefers Azure Trusted Signing,
# which signs the same files with its own action and needs no file; this is
# the path for an OV certificate a CA issued, or for a maintainer's machine.
#
# Needs, in the environment:
#   TIAMAT_SIGN_PFX           path to the .pfx
#   TIAMAT_SIGN_PFX_PASSWORD  its password
#
# Usage: scripts/sign-windows.ps1 -Stage <staged dir>
param(
    [Parameter(Mandatory = $true)][string]$Stage
)
$ErrorActionPreference = "Stop"
if (-not $env:TIAMAT_SIGN_PFX) { throw "TIAMAT_SIGN_PFX is not set" }
if (-not $env:TIAMAT_SIGN_PFX_PASSWORD) { throw "TIAMAT_SIGN_PFX_PASSWORD is not set" }

# signtool ships with the Windows SDK and is not on PATH; the newest kit wins.
$signtool = Get-Command signtool -ErrorAction SilentlyContinue
if ($signtool) { $signtool = $signtool.Source }
else {
    $kits = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending
    foreach ($kit in $kits) {
        $candidate = Join-Path $kit.FullName "x64\signtool.exe"
        if (Test-Path $candidate) { $signtool = $candidate; break }
    }
}
if (-not $signtool) { throw "signtool.exe not found: install the Windows SDK" }

$targets = @(Join-Path $Stage "tiamat.exe") + (Get-ChildItem (Join-Path $Stage "current") -Filter *.exe | ForEach-Object { $_.FullName })
foreach ($exe in $targets) {
    if (-not (Test-Path $exe)) { continue }
    # SHA-256 digest, an RFC 3161 timestamp so the signature outlives the
    # certificate, and a verify straight after.
    & $signtool sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com /f $env:TIAMAT_SIGN_PFX /p $env:TIAMAT_SIGN_PFX_PASSWORD $exe
    if ($LASTEXITCODE -ne 0) { throw "signing failed: $exe" }
    & $signtool verify /pa /q $exe
    if ($LASTEXITCODE -ne 0) { throw "verification failed: $exe" }
    Write-Host "signed $exe"
}
New-Item -ItemType File -Path (Join-Path $Stage ".signed") -Force | Out-Null
Write-Host "==> signed: $Stage"
