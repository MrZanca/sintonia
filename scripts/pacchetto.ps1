# Crea lo zip portatile di Sintonia per Windows (x64) in dist\.
# Uso: powershell -ExecutionPolicy Bypass -File scripts\pacchetto.ps1 [-Prova]
# Contenuto: sintonia.exe, libmpv-2.dll (LGPL), licenze. L'installer vero arriva in M4.
# -Prova aggiunge finto_xtream.exe (finto server Xtream con dati inventati): solo per le versioni di prova.

param([switch]$Prova)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
cargo build --release -p sintonia-app
if ($LASTEXITCODE -ne 0) { throw "compilazione non riuscita" }
if ($Prova) {
    cargo build --release -p sintonia-core --features mock --example finto_xtream
    if ($LASTEXITCODE -ne 0) { throw "compilazione del finto server non riuscita" }
}

$name = "Sintonia-$version-windows-x64"
$stage = Join-Path $root "dist\$name"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item target\release\sintonia.exe $stage
Copy-Item target\release\libmpv-2.dll $stage
Copy-Item LICENSE (Join-Path $stage 'LICENSE.txt')
Copy-Item THIRD-PARTY.md $stage
if ($Prova) { Copy-Item target\release\examples\finto_xtream.exe $stage }
@"
Sintonia $version - versione portatile di prova

Avvio: doppio clic su sintonia.exe (non serve installare niente).
Windows potrebbe mostrare "PC protetto": l'app non è firmata. Scegli "Ulteriori informazioni" e "Esegui comunque".

Sintonia non contiene canali: aggiungi la tua playlist M3U o il tuo account Xtream Codes da
Impostazioni > Sorgenti.
I dati restano sul tuo PC (%APPDATA%\Sintonia); gli indirizzi dei canali sono cifrati, con la chiave
nella Gestione credenziali di Windows.

Licenze: Sintonia GPL-3.0 (LICENSE.txt); libmpv LGPL, vedi THIRD-PARTY.md.
"@ | Set-Content -Path (Join-Path $stage 'LEGGIMI.txt') -Encoding utf8
if ($Prova) {
    @"

Prova di film e serie senza un account vero
-------------------------------------------
1. Doppio clic su finto_xtream.exe e lascia aperta la sua finestra.
2. In Sintonia: Impostazioni > Sorgenti > Xtream Codes
   server http://127.0.0.1:8899   utente mario   password prova123
   poi Verifica, Salva e Usa.
Il catalogo è inventato; i video sono flussi di prova pubblici. Chiudi la finestra per fermarlo.
"@ | Add-Content -Path (Join-Path $stage 'LEGGIMI.txt') -Encoding utf8
}

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content -Path "$zip.sha256" -Encoding ascii
Write-Output "$zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
Write-Output "SHA-256: $hash"
