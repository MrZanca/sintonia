# Scarica la libmpv LGPL per Android prodotta dal workflow «libmpv Android», ne controlla lo SHA-256
# e la mette in vendor\mpv-android\<abi>\ (le licenze e BUILDINFO.txt in vendor\mpv-android\).
# Serve la CLI di GitHub (gh) con l'accesso al repository.
#
#   powershell -ExecutionPolicy Bypass -File scripts\android-libmpv.ps1 -Run <id> -Sha256 <hash>
#
# Lo SHA-256 atteso è quello scritto in docs/sviluppo.md, non quello che arriva con il file.
param(
    [Parameter(Mandatory)] [string] $Run,
    [Parameter(Mandatory)] [string] $Sha256
)
$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$tmp = Join-Path ([IO.Path]::GetTempPath()) "sintonia-libmpv-$Run"
if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
New-Item -ItemType Directory $tmp | Out-Null

gh run download $Run --repo MrZanca/sintonia --name libmpv-android-lgpl --dir $tmp
if ($LASTEXITCODE -ne 0) { throw "download dell'esecuzione $Run non riuscito" }

$zip = Join-Path $tmp 'libmpv-android-lgpl.zip'
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
if ($hash -ne $Sha256.ToLower()) { throw "SHA-256 diverso: atteso $Sha256, trovato $hash" }

$dest = Join-Path $root 'vendor\mpv-android'
# la build GPL della fase 0 resta da parte, solo per confronti interni
if ((Test-Path $dest) -and -not (Test-Path (Join-Path $dest 'BUILDINFO.txt'))) {
    $gpl = Join-Path $root 'vendor\mpv-android-gpl'
    if (Test-Path $gpl) { Remove-Item $gpl -Recurse -Force }
    Move-Item $dest $gpl
    Write-Output "build GPL spostata in $gpl"
}
if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
Expand-Archive $zip $dest
Remove-Item $tmp -Recurse -Force

Get-Content (Join-Path $dest 'BUILDINFO.txt') -TotalCount 12
Write-Output "libmpv LGPL in $dest (SHA-256 $hash)"
