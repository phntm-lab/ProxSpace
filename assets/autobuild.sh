#!/usr/bin/env bash
# Build every proxmark3 checkout under /pm3 and pack each result into /builds.
#
# Run by `proxspace autobuild`, which mounts /builds, makes sure p7zip is there
# and then hands over. The build logic stays here because it is proxmark3's,
# not ProxSpace's: it follows whatever the makefiles of a checkout need, and
# that is shell work.
#
# Nothing here installs or changes anything outside /tmp and /builds.

# The subsystem this environment is built for. Run from any other one, the
# client would be linked against a different C runtime than the DLLs collected
# below, and the archive would only fail on the machine it is unpacked on.
if [ "$MSYSTEM" != "UCRT64" ]; then
	echo "autobuild must run in the UCRT64 shell, not ${MSYSTEM:-none}" >&2
	exit 1
fi

pm3Dir=/pm3
# Where the archives go, handed over by `proxspace autobuild` as the drive path
# the shell sees. It is passed rather than taken from the /builds mount because
# cygwin builds its mount table once per installation and every later process
# reuses it: with any msys2 process still running — an open shell, a gpg-agent
# left behind by a package install — a mount added moments ago is invisible,
# and /builds would silently be an ordinary directory inside the tree.
copyDir="${1:-/builds}"
buildDir=/tmp
# Toolchain prefix, /ucrt64. Taken from the shell, which derives it from
# MSYSTEM, so that there is one answer to what UCRT64 means and not two.
prefixDir=${MSYSTEM_PREFIX:-/ucrt64}
# Templates that go into every archive, materialised from the binary.
assetDir=/opt/proxspace/autobuild

function copy_shell {
	DEPLIST=(
			"/usr/bin/bash.exe"
			"/usr/bin/dirname.exe"
			"/usr/bin/basename.exe"
			"/usr/bin/uname.exe"
			"/usr/bin/awk.exe"
			"/usr/bin/grep.exe"
			"/usr/bin/sleep.exe"
			)
	mkdir -p "$dstDir/client/libs/shell"
	for dep in ${DEPLIST[*]}; do
		ldd $dep | grep "=> /usr" | awk '{print $3}' | xargs -I '{}' cp -v '{}' "$dstDir/client/libs/shell"
		cp $dep "$dstDir/client/libs/shell"
	done

	#tmp dir required for bash
	mkdir -p "$dstDir/client/tmp"
}

function copy_common {
	rm -rf "$dstDir"
	mkdir -p "$copyDir/$buildName/"
	mkdir -p "$dstDir/client"
	mkdir -p "$dstDir/client/libs"
	mkdir -p "$dstDir/recovery"
	mkdir -p "$dstDir/Windows Driver (not required for Windows 10)"

	#Copy required libraries to client/libs
	ldd "$srcDir/client/proxmark3.exe" | grep "=> $prefixDir" | awk '{print $3}' | xargs -I '{}' cp -v '{}' "$dstDir/client/libs"
	#Copy qt6 platform dll
	cp "$prefixDir/share/qt6/plugins/platforms/qwindows.dll" "$dstDir/client/libs"
	#Copy firmware
	cp "$srcDir/armsrc/obj/fullimage.elf" "$dstDir/client"
	cp "$srcDir/bootrom/obj/bootrom.elf" "$dstDir/client"
	#Copy recovery images. Whatever is there: the names have changed upstream
	#more than once, and a fixed list quietly ships an archive without them.
	cp $srcDir/recovery/*.bin "$dstDir/recovery"
	#Copy driver
	cp "$srcDir/driver/proxmark3.inf" "$dstDir/Windows Driver (not required for Windows 10)"
}

function check_for_updates {
	git fetch
	git pull --ff-only
	hash=$(git rev-parse HEAD)

	if ls $copyDir/$buildName/*-$hash.7z 1> /dev/null 2>&1; then
		return 1 #build exist
	else
		return 0 #build doesn't exist
	fi
}

function zip_folder {
	date=$(date +%Y%m%d)
	7z a -r -mx9 $copyDir/$buildName/$buildName-$date-$hash.7z $dstDir/*
}

function build_rrg {
	make clean
	#Running python scripts outside ProxSpace is a bad idea
	make SKIPPYTHON=1 -j
	if [ $? -eq 0 ]; then
		copy_common
		copy_shell

		#Copy contents of the autobuild folder
		cp -r $assetDir/rrg/* "$dstDir"

		#Copy the client and additional files
		cp -r $srcDir/client/{proxmark3.exe,lualibs,luascripts,cmdscripts,dictionaries,resources} "$dstDir/client"

		#Copy the pm3 scripts
		cp -r $srcDir/{pm3,pm3-flash,pm3-flash-all,pm3-flash-bootrom,pm3-flash-fullimage} "$dstDir/client"

		zip_folder
	fi
}

function build_official {
	make clean
	make
	if [ $? -eq 0 ]; then
		copy_common

		#Copy contents of the autobuild folder
		cp -r $assetDir/official/* "$dstDir"

		#Copy the client and additional files
		cp -r $srcDir/client/{proxmark3.exe,flasher.exe,*.dic,lualibs,scripts,hardnested} "$dstDir/client"

		#Remove accidentally copied .h/.c files from hardnested folder
		rm $dstDir/client/hardnested/{*.h,*.c}

		zip_folder
	fi
}

function loop_folders {
	for i in $( ls -d */ ); do
		buildName=${i%%/}
		srcDir=$pm3Dir/$buildName
		dstDir=$buildDir/$buildName
		echo Processing: $srcDir
		cd $srcDir

		if check_for_updates; then
			#Build rrg
			if [ -f "pm3" ]; then
				build_rrg
			else
				build_official
			fi
		fi
	done
}

cd $pm3Dir
loop_folders
