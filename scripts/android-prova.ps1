# Compila e impacchetta la prova Android di M5 fase 0 (prototipi\android-fase0) in un APK di debug.
# Uso: powershell -ExecutionPolicy Bypass -File scripts\android-prova.ps1 [-Abi arm64-v8a|x86_64] [-Installa]
# Serve l'ambiente Android descritto in docs\sviluppo.md (SDK, NDK, JDK 17, cargo-ndk) e le librerie
# di mpv in vendor\mpv-android\<abi>\. Firma con la chiave di debug di Android (password "android":
# non è un segreto). La chiave vera dell'APK di rilascio non passa da qui (fase 5).
# -Installa lo installa sul dispositivo collegato con adb.

param([string]$Abi = 'arm64-v8a', [switch]$Installa)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$proto = Join-Path $root 'prototipi\android-fase0'
$android = Join-Path $env:LOCALAPPDATA 'Android'
if (-not $env:ANDROID_HOME) { $env:ANDROID_HOME = Join-Path $android 'sdk' }
if (-not $env:ANDROID_NDK_HOME) { $env:ANDROID_NDK_HOME = Join-Path $android 'sdk\ndk\27.2.12479018' }
if (-not $env:JAVA_HOME) { $env:JAVA_HOME = Join-Path $android 'jdk17' }
$env:PATH = "$env:JAVA_HOME\bin;$env:PATH"
$tools = Join-Path $env:ANDROID_HOME 'build-tools\34.0.0'
$platform = Join-Path $env:ANDROID_HOME 'platforms\android-34\android.jar'
# il build script del backend Android di Slint cerca android.jar da solo e non sempre lo trova
$env:ANDROID_JAR = $platform

$triple = @{ 'arm64-v8a' = 'aarch64-linux-android'; 'x86_64' = 'x86_64-linux-android' }[$Abi]
if (-not $triple) { throw "ABI non prevista: $Abi" }
$mpv = if ($env:SINTONIA_MPV_DIR) { $env:SINTONIA_MPV_DIR } else { Join-Path $root 'vendor\mpv-android' }
$mpvAbi = Join-Path $mpv $Abi
if (-not (Test-Path (Join-Path $mpvAbi 'libmpv.so'))) { throw "libmpv.so non trovata in $mpvAbi (vedi docs\sviluppo.md)" }

Push-Location $proto
try {
    cargo ndk -t $Abi build --release
    if ($LASTEXITCODE -ne 0) { throw 'compilazione non riuscita' }
} finally { Pop-Location }

$out = Join-Path $proto "target\apk-$Abi"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force $out | Out-Null

# classi Java dell'aiuto di Slint (tastiera, insets): il telefono vuole un classes.dex
$javaOut = Get-ChildItem (Join-Path $proto "target\$triple\release\build") -Directory -Filter 'i-slint-backend-android-activity-*' |
    ForEach-Object { Join-Path $_.FullName 'out\java' } | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $javaOut) { throw 'classi Java del backend Android di Slint non trovate' }
# d8 su Windows non regge la riga di comando con tutte le classi: le si passa in un jar
$jar = Join-Path $out 'slint-helper.jar'
& jar cf $jar -C $javaOut .
if ($LASTEXITCODE -ne 0) { throw 'jar non riuscito' }
& (Join-Path $tools 'd8.bat') --release --lib $platform --output $out $jar
if ($LASTEXITCODE -ne 0) { throw 'd8 non riuscito' }

$base = Join-Path $out 'base.apk'
& (Join-Path $tools 'aapt2.exe') link --manifest (Join-Path $proto 'AndroidManifest.xml') -I $platform -o $base
if ($LASTEXITCODE -ne 0) { throw 'aapt2 non riuscito' }

# librerie native senza compressione: Android le carica direttamente dall'APK (zipalign le allinea)
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::Open($base, 'Update')
try {
    $none = [System.IO.Compression.CompressionLevel]::NoCompression
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $out 'classes.dex'), 'classes.dex')
    $libs = @(Get-ChildItem (Join-Path $mpvAbi '*.so')) +
        @(Get-Item (Join-Path $proto "target\$triple\release\libsintonia_android_prova.so"))
    foreach ($lib in $libs) {
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $lib.FullName, "lib/$Abi/$($lib.Name)", $none)
    }
} finally { $zip.Dispose() }

$aligned = Join-Path $out 'sintonia-prova.apk'
& (Join-Path $tools 'zipalign.exe') -f -p 4 $base $aligned
if ($LASTEXITCODE -ne 0) { throw 'zipalign non riuscito' }

$keystore = Join-Path $env:USERPROFILE '.android\debug.keystore'
if (-not (Test-Path $keystore)) {
    New-Item -ItemType Directory -Force (Split-Path $keystore) | Out-Null
    & keytool -genkeypair -keystore $keystore -storepass android -keypass android -alias androiddebugkey `
        -keyalg RSA -keysize 2048 -validity 10000 -dname 'CN=Android Debug,O=Android,C=US'
}
& (Join-Path $tools 'apksigner.bat') sign --ks $keystore --ks-pass pass:android --key-pass pass:android $aligned
if ($LASTEXITCODE -ne 0) { throw 'apksigner non riuscito' }
Write-Host "APK: $aligned ($([math]::Round((Get-Item $aligned).Length / 1MB, 1)) MB)"

if ($Installa) {
    adb install -r $aligned
    if ($LASTEXITCODE -ne 0) { throw 'adb install non riuscito (vedi docs\sviluppo.md, Xiaomi)' }
}
