# Compila Sintonia per Android e la impacchetta in un APK firmato con la chiave di debug.
# Uso: powershell -ExecutionPolicy Bypass -File scripts\android-app.ps1 [-Abi arm64-v8a|x86_64] [-Installa] [-Avvia]
# Serve l'ambiente Android descritto in docs\sviluppo.md (SDK, NDK, JDK 17, cargo-ndk) e la libmpv
# LGPL in vendor\mpv-android\<abi>\ (scripts\android-libmpv.ps1) o in SINTONIA_MPV_DIR.
# L'APK è "debuggable" (adb shell run-as legge i dati) e firmato con la chiave di debug di Android
# (password "android": non è un segreto). La chiave vera e l'APK di rilascio arrivano con la fase 5.

param([string]$Abi = 'arm64-v8a', [switch]$Installa, [switch]$Avvia)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$crate = Join-Path $root 'crates\sintonia-app'
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

# versione dal Cargo.toml del workspace: X.Y.Z -> versionCode X*10000 + Y*100 + Z
$cargo = Get-Content -Raw (Join-Path $root 'Cargo.toml')
if ($cargo -notmatch '(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"') { throw 'versione non trovata in Cargo.toml' }
$version = "$($Matches[1]).$($Matches[2]).$($Matches[3])"
$versionCode = [int]$Matches[1] * 10000 + [int]$Matches[2] * 100 + [int]$Matches[3]

# la libreria dell'app come cdylib: nel Cargo.toml è solo rlib, perché sul PC non serve
Push-Location $root
try {
    cargo ndk -t $Abi rustc -p sintonia-app --lib --release --crate-type cdylib
    if ($LASTEXITCODE -ne 0) { throw 'compilazione non riuscita' }
} finally { Pop-Location }
$lib = Join-Path $root "target\$triple\release\libsintonia_app.so"

$out = Join-Path $root "target\android-apk\$Abi"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
$classes = Join-Path $out 'classes'
New-Item -ItemType Directory -Force $classes | Out-Null

# classi Java: l'aiuto di Slint (tastiera, bordi) e la nostra activity, in un solo classes.dex
$slintJava = Get-ChildItem (Join-Path $root "target\$triple\release\build") -Directory -Filter 'i-slint-backend-android-activity-*' |
    Sort-Object LastWriteTime -Descending |
    ForEach-Object { Join-Path $_.FullName 'out\java' } | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $slintJava) { throw 'classi Java del backend Android di Slint non trovate' }
Copy-Item -Recurse (Join-Path $slintJava '*') $classes
# @(...): con un solo file PowerShell darebbe una stringa, e javac riceverebbe i suoi caratteri
$sources = @(Get-ChildItem -Recurse (Join-Path $crate 'android\java') -Filter '*.java' | ForEach-Object { $_.FullName })
& javac -source 8 -target 8 '-Xlint:-options' -encoding UTF-8 -bootclasspath $platform -d $classes $sources
if ($LASTEXITCODE -ne 0) { throw 'javac non riuscito' }
# d8 su Windows non regge la riga di comando con tutte le classi: le si passa in un jar
$jar = Join-Path $out 'classes.jar'
& jar cf $jar -C $classes .
if ($LASTEXITCODE -ne 0) { throw 'jar non riuscito' }
& (Join-Path $tools 'd8.bat') --release --min-api 26 --lib $platform --output $out $jar
if ($LASTEXITCODE -ne 0) { throw 'd8 non riuscito' }

$base = Join-Path $out 'base.apk'
& (Join-Path $tools 'aapt2.exe') link --manifest (Join-Path $crate 'android\AndroidManifest.xml') -I $platform `
    --version-name $version --version-code $versionCode --debug-mode -o $base
if ($LASTEXITCODE -ne 0) { throw 'aapt2 non riuscito' }

# librerie native senza compressione: Android le carica direttamente dall'APK (zipalign le allinea)
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::Open($base, 'Update')
try {
    $none = [System.IO.Compression.CompressionLevel]::NoCompression
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $out 'classes.dex'), 'classes.dex')
    $libs = @(Get-ChildItem (Join-Path $mpvAbi '*.so')) + @(Get-Item $lib)
    foreach ($so in $libs) {
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $so.FullName, "lib/$Abi/$($so.Name)", $none)
    }
} finally { $zip.Dispose() }

$aligned = Join-Path $out "Sintonia-$version-android-$Abi-debug.apk"
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
Write-Host "APK: $aligned ($([math]::Round((Get-Item $aligned).Length / 1MB, 1)) MB, versione $version)"

if ($Installa) {
    adb install -r $aligned
    if ($LASTEXITCODE -ne 0) { throw 'adb install non riuscito (vedi docs\sviluppo.md, Xiaomi)' }
}
if ($Avvia) {
    adb shell am start -n io.github.mrzanca.sintonia/.SintoniaActivity
}
