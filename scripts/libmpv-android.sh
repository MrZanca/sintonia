#!/bin/bash -e
# Compila libmpv **LGPL** per Android (arm64-v8a e x86_64) con le buildscripts di mpv-android.
# Gira su Linux (Ubuntu 24.04, come il workflow .github/workflows/libmpv-android.yml); non su Windows.
#
# Rispetto a mpv-android cambiano solo due cose, entrambe di licenza: FFmpeg senza --enable-gpl
# (resta --enable-version3, perché mbedtls è Apache-2.0 e FFmpeg lo ammette solo come LGPLv3) e mpv
# con -Dgpl=false. Tutte le sorgenti sono fissate a una versione o a un commit, e alla fine lo
# script controlla davvero la licenza delle librerie prodotte: se qualcosa è GPL si ferma.
#
# Uscita: $OUT/libmpv-android-lgpl.zip (cartelle <abi>/ con le .so, LICENSES/, BUILDINFO.txt) e
# il suo .sha256. Le variabili sotto si possono cambiare dall'ambiente.

: "${MPV_ANDROID_COMMIT:=255901dd586fa750000fa0075f69d6ffdd26cd9c}"
# stesso commit di mpv della build per Windows (docs/sviluppo.md)
: "${MPV_COMMIT:=6f605372b9c0c397d60d3464e6fcb12e71f95d9e}"
: "${DAV1D_TAG:=1.5.4}"
: "${LIBASS_TAG:=0.17.5}"
# la prima versione che il commit di mpv accetta (meson.build: >= 7.360.1)
: "${LIBPLACEBO_TAG:=v7.360.1}"
: "${ARCHS:=arm64 x86_64}"
: "${WORK:=$PWD/libmpv-android-work}"
: "${OUT:=$PWD/libmpv-android-out}"

msg() { printf '\n==> %s\n' "$1"; }
die() { printf '\nERRORE: %s\n' "$1" >&2; exit 1; }

# scarica un solo commit o tag, senza la storia
fetch_git() { # url cartella rif [--recurse-submodules]
	git init -q "$2"
	git -C "$2" remote add origin "$1"
	git -C "$2" fetch -q --depth 1 origin "$3"
	git -C "$2" checkout -q FETCH_HEAD
	if [ "$4" = --recurse-submodules ]; then
		git -C "$2" submodule update -q --init --recursive --depth 1
	fi
}

mkdir -p "$WORK" "$OUT"
cd "$WORK"

msg "mpv-android $MPV_ANDROID_COMMIT"
[ -d mpv-android ] || fetch_git https://github.com/mpv-android/mpv-android mpv-android "$MPV_ANDROID_COMMIT"
cd mpv-android/buildscripts
. ./include/depinfo.sh

msg "Licenza: FFmpeg senza --enable-gpl, mpv con -Dgpl=false"
if ! grep -qF -- '-Dgpl=false' scripts/mpv.sh; then
	grep -qF -- '--enable-{gpl,version3}' scripts/ffmpeg.sh ||
		die "ffmpeg.sh è cambiato: non trovo --enable-{gpl,version3}"
	grep -qF -- '-Dlibmpv=true' scripts/mpv.sh || die "mpv.sh è cambiato: non trovo -Dlibmpv=true"
	sed -i 's/--enable-{gpl,version3}/--enable-version3/' scripts/ffmpeg.sh
	sed -i 's/-Dlibmpv=true/-Dlibmpv=true -Dgpl=false/' scripts/mpv.sh
fi
! grep -qE -- 'enable-(gpl|nonfree)|enable-\{gpl' scripts/ffmpeg.sh || die "ffmpeg.sh chiede ancora la GPL"
grep -qF -- '-Dgpl=false' scripts/mpv.sh || die "mpv.sh senza -Dgpl=false"

msg "SDK e NDK $v_ndk"
if [[ -n "$ANDROID_HOME" && -d "$ANDROID_HOME" && ! -e sdk/android-sdk-linux ]]; then
	mkdir -p sdk
	ln -s "$ANDROID_HOME" sdk/android-sdk-linux
fi
IN_CI=1 ./include/download-sdk.sh

msg "Sorgenti"
mkdir -p deps
[ -d deps/mpv ] || fetch_git https://github.com/mpv-player/mpv deps/mpv "$MPV_COMMIT"
[ -d deps/dav1d ] || fetch_git https://github.com/videolan/dav1d deps/dav1d "refs/tags/$DAV1D_TAG"
[ -d deps/libass ] || fetch_git https://github.com/libass/libass deps/libass "refs/tags/$LIBASS_TAG"
[ -d deps/libplacebo ] ||
	fetch_git https://github.com/haasn/libplacebo deps/libplacebo "refs/tags/$LIBPLACEBO_TAG" --recurse-submodules
# il resto (FFmpeg $v_ci_ffmpeg e le librerie in archivio) alle versioni di depinfo.sh
IN_CI=1 WGET="wget -q" ./include/download-deps.sh

for arch in $ARCHS; do
	msg "Compilazione ($arch)"
	./buildall.sh --arch "$arch" mpv || {
		log="deps/mpv/_build_$arch/meson-logs/meson-log.txt"
		[ -f "$log" ] && tail -n 200 "$log"
		exit 1
	}
done

toolchain=$(echo "$PWD/sdk/android-ndk-$v_ndk/toolchains/llvm/prebuilt/"*)
readelf="$toolchain/bin/llvm-readelf"
strip="$toolchain/bin/llvm-strip"

pkg="$OUT/libmpv-android-lgpl"
rm -rf "$pkg"
mkdir -p "$pkg/LICENSES"
info="$pkg/BUILDINFO.txt"
{
	echo "libmpv LGPL per Android, compilata con scripts/libmpv-android.sh di Sintonia"
	echo "data: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
	echo "mpv-android (buildscripts): $MPV_ANDROID_COMMIT"
	echo "mpv: $MPV_COMMIT"
	echo "FFmpeg: $v_ci_ffmpeg ($(git -C deps/ffmpeg rev-parse HEAD))"
	echo "dav1d: $DAV1D_TAG ($(git -C deps/dav1d rev-parse HEAD))"
	echo "libass: $LIBASS_TAG ($(git -C deps/libass rev-parse HEAD))"
	echo "libplacebo: $LIBPLACEBO_TAG ($(git -C deps/libplacebo rev-parse HEAD))"
	echo "NDK: $v_ndk ($v_ndk_n), API minima 23"
	echo "mbedtls $v_mbedtls, libxml2 $v_libxml2, freetype $v_freetype, fontconfig $v_fontconfig,"
	echo "fribidi $v_fribidi, harfbuzz $v_harfbuzz, libunibreak $v_unibreak, lua $v_lua, curl $v_curl"
} >"$info"

for arch in $ARCHS; do
	case $arch in
		arm64) abi=arm64-v8a triple=aarch64-linux-android ;;
		x86_64) abi=x86_64 triple=x86_64-linux-android ;;
		*) die "architettura $arch non prevista" ;;
	esac
	msg "Controllo della licenza ($arch)"
	ffconf="deps/ffmpeg/_build_$arch/config.h"
	mpvconf="deps/mpv/_build_$arch/config.h"
	grep -qxF '#define FFMPEG_LICENSE "LGPL version 3 or later"' "$ffconf" ||
		die "FFmpeg ($arch) non è LGPLv3: $(grep FFMPEG_LICENSE "$ffconf")"
	ffcfg=$(grep '^#define FFMPEG_CONFIGURATION ' "$ffconf")
	if grep -qE -- '--enable-(gpl|nonfree)|libx26[45]|libxvid|librubberband|libvidstab|frei0r' <<<"$ffcfg"; then
		die "FFmpeg ($arch) ha componenti GPL o non liberi: $ffcfg"
	fi
	grep -qxF '#define HAVE_GPL 0' "$mpvconf" || die "mpv ($arch) non è stato compilato con -Dgpl=false"

	prefix="prefix/$arch"
	mkdir -p "$pkg/$abi"
	# cp segue i collegamenti simbolici: nel pacchetto finiscono file veri con i nomi semplici
	cp "$prefix"/lib/libmpv.so "$prefix"/lib/libav*.so "$prefix"/lib/libsw*.so "$pkg/$abi/"
	cp "$toolchain/sysroot/usr/lib/$triple/libc++_shared.so" "$pkg/$abi/"
	# le librerie restano dinamiche e sostituibili (LGPL); si toglie solo il superfluo
	"$strip" --strip-unneeded "$pkg/$abi"/*.so
	for so in "$pkg/$abi"/*.so; do
		needed=$("$readelf" -d "$so" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
		# ogni dipendenza è nel pacchetto o è una libreria di sistema di Android (stub dell'NDK);
		# un nome con la versione (libavcodec.so.62) non si caricherebbe dall'APK
		for lib in $needed; do
			[ -f "$pkg/$abi/$lib" ] || [ -f "$toolchain/sysroot/usr/lib/$triple/23/$lib" ] ||
				die "$(basename "$so") ($arch) dipende da $lib, che non c'è"
		done
	done
	{
		echo
		echo "[$abi]"
		echo "FFmpeg: $(sed -n 's/^#define FFMPEG_LICENSE //p' "$ffconf")"
		echo "FFmpeg configure: ${ffcfg#\#define FFMPEG_CONFIGURATION }"
		echo "mpv: HAVE_GPL 0"
		echo "librerie statiche dentro libmpv.so: $(cd "$prefix/lib" && ls ./*.a | xargs -n1 basename | tr '\n' ' ')"
		echo "dipendenze di libmpv.so: $("$readelf" -d "$pkg/$abi/libmpv.so" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | tr '\n' ' ')"
		(cd "$pkg/$abi" && sha256sum ./*.so)
	} >>"$info"
done

msg "Licenze"
# una licenza mancante non butta via la compilazione: finisce in BUILDINFO.txt e si sistema a mano
licence() { # sorgente nome
	cp "$1" "$pkg/LICENSES/$2" 2>/dev/null || { echo "MANCA la licenza $1" | tee -a "$info" >&2; }
}
licence deps/mpv/LICENSE.LGPL mpv-LGPL-2.1.txt
licence deps/mpv/Copyright mpv-Copyright.txt
licence deps/ffmpeg/COPYING.LGPLv3 ffmpeg-LGPL-3.0.txt
licence deps/ffmpeg/LICENSE.md ffmpeg-LICENSE.md
licence deps/dav1d/COPYING dav1d-BSD-2-Clause.txt
licence deps/libass/COPYING libass-ISC.txt
licence deps/libplacebo/LICENSE libplacebo-LGPL-2.1.txt
licence deps/mbedtls/LICENSE mbedtls-Apache-2.0.txt
licence deps/libxml2/Copyright libxml2-MIT.txt
licence deps/freetype2/LICENSE.TXT freetype-LICENSE.txt
licence deps/freetype2/docs/FTL.TXT freetype-FTL.txt
licence deps/fontconfig/COPYING fontconfig.txt
licence deps/fribidi/COPYING fribidi-LGPL-2.1.txt
licence deps/harfbuzz/COPYING harfbuzz-MIT.txt
licence deps/unibreak/LICENCE libunibreak-zlib.txt
licence deps/curl/COPYING curl.txt
licence deps/lua/doc/readme.html lua-readme.html
licence "$PWD/sdk/android-ndk-$v_ndk/NOTICE.toolchain" ndk-toolchain-NOTICE.txt

cd "$OUT"
rm -f libmpv-android-lgpl.zip libmpv-android-lgpl.zip.sha256
(cd libmpv-android-lgpl && zip -qr -X ../libmpv-android-lgpl.zip .)
sha256sum libmpv-android-lgpl.zip | tee libmpv-android-lgpl.zip.sha256
cat "$info"
