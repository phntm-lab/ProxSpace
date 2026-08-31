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

# Where the checkouts are looked for.
sources=/pm3
# Where the finished archives go, handed over by `proxspace autobuild` as the
# drive path the shell sees. It is passed rather than taken from the /builds
# mount because cygwin builds its mount table once per installation and every
# later process reuses it: with any msys2 process still running — an open
# shell, a gpg-agent left behind by a package install — a mount added moments
# ago is invisible, and /builds would silently be an ordinary directory inside
# the tree.
archives="${1:-/builds}"
# Where an archive is assembled before it is packed.
staging=/tmp
# Toolchain prefix, /ucrt64. Taken from the shell, which derives it from
# MSYSTEM, so that there is one answer to what UCRT64 means and not two.
prefix=${MSYSTEM_PREFIX:-/ucrt64}
# Batch files that go into every archive, materialised from the binary.
templates=/opt/proxspace/autobuild

# Everything both forks put into an archive: the libraries the client was
# linked against, the firmware it flashes, and the driver older Windows asks
# for. The client itself differs between forks and is copied by the callers.
collect_common() {
    rm -rf "$layout"
    mkdir -p "$archives/$name/"
    mkdir -p "$layout/client/libs"
    mkdir -p "$layout/recovery"
    mkdir -p "$layout/Windows Driver (not required for Windows 10)"

    # The client's own DLLs, read off the executable rather than listed, and
    # the Qt platform plugin, which is loaded by name at runtime and so is
    # invisible to ldd.
    ldd "$source/client/proxmark3.exe" |
        grep "=> $prefix" |
        awk '{print $3}' |
        xargs -I '{}' cp -v '{}' "$layout/client/libs"
    cp "$prefix/share/qt6/plugins/platforms/qwindows.dll" "$layout/client/libs"

    cp "$source/armsrc/obj/fullimage.elf" "$layout/client"
    cp "$source/bootrom/obj/bootrom.elf" "$layout/client"
    # Whatever recovery images the checkout produced: their names have changed
    # upstream more than once, and a fixed list quietly ships an archive
    # without them.
    cp "$source"/recovery/*.bin "$layout/recovery"
    cp "$source/driver/proxmark3.inf" "$layout/Windows Driver (not required for Windows 10)"
}

# The handful of msys2 programs the RRG client shells out to, with their own
# DLLs. Without them its scripts find no bash on a machine that has never had
# msys2 on it.
collect_shell() {
    local tools=(
        /usr/bin/bash.exe
        /usr/bin/dirname.exe
        /usr/bin/basename.exe
        /usr/bin/uname.exe
        /usr/bin/awk.exe
        /usr/bin/grep.exe
        /usr/bin/sleep.exe
    )

    mkdir -p "$layout/client/libs/shell"
    for tool in "${tools[@]}"; do
        ldd "$tool" |
            grep "=> /usr" |
            awk '{print $3}' |
            xargs -I '{}' cp -v '{}' "$layout/client/libs/shell"
        cp "$tool" "$layout/client/libs/shell"
    done

    # bash refuses to run without somewhere to put its temporary files.
    mkdir -p "$layout/client/tmp"
}

# Bring the checkout up to date and say whether this commit still needs
# building. The commit it settled on is left in $commit for the archive name.
needs_building() {
    git fetch
    git pull --ff-only
    commit=$(git rev-parse HEAD)

    ! ls "$archives/$name"/*-"$commit".7z >/dev/null 2>&1
}

pack() {
    7z a -r -mx9 "$archives/$name/$name-$(date +%Y%m%d)-$commit.7z" "$layout"/*
}

# The RRG client, which runs from its pm3 scripts and therefore needs a shell.
build_rrg() {
    make clean
    # Python scripts of a checkout expect the ProxSpace python, not one found
    # on the machine the archive is unpacked on.
    if make SKIPPYTHON=1 -j; then
        collect_common
        collect_shell

        cp -r "$templates"/rrg/* "$layout"
        cp -r "$source"/client/{proxmark3.exe,lualibs,luascripts,cmdscripts,dictionaries,resources} "$layout/client"
        cp -r "$source"/{pm3,pm3-flash,pm3-flash-all,pm3-flash-bootrom,pm3-flash-fullimage} "$layout/client"

        pack
    fi
}

# The official client, which is started from batch files and ships its own
# flasher.
build_official() {
    make clean
    if make; then
        collect_common

        cp -r "$templates"/official/* "$layout"
        cp -r "$source"/client/{proxmark3.exe,flasher.exe,*.dic,lualibs,scripts,hardnested} "$layout/client"
        # hardnested/ carries its sources next to its tables; they are of no
        # use to anyone unpacking a build.
        rm "$layout"/client/hardnested/{*.h,*.c}

        pack
    fi
}

cd "$sources" || exit 1
for directory in */; do
    name=${directory%%/}
    source=$sources/$name
    layout=$staging/$name
    echo "Processing: $source"
    cd "$source" || continue

    if needs_building; then
        # A checkout with a pm3 script in its root is the RRG fork; the
        # official one has none.
        if [ -f "pm3" ]; then
            build_rrg
        else
            build_official
        fi
    fi
done
