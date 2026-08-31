# Security

## Supported versions

ProxSpace is one executable with no branches to maintain: the latest release is
the supported one, and a fix ships as the next release rather than as a patch
to an older tag.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Anything older | No — update first, then report if it still happens |
| `dev` branch | Yes, but expect it to be fixed by a commit rather than a release |

## Reporting a vulnerability

Write to [@Lemn4t on Telegram](https://t.me/Lemn4t), directly rather than in
any public channel, and say in the first message that it is a security report.
Nothing about a working attack should be posted anywhere public before there is
a release that fixes it.

Please include what `proxspace info` writes to `proxspace-info.txt` and, if the
problem happened during an install, `proxspace.log` from next to the
executable. Expect an answer within a few days. If the report holds up, the fix
and the release that carries it are credited to you unless you would rather
they were not.

## What the attack surface actually is

Most of what ProxSpace does is fetch things from the internet and run them,
which is worth being precise about.

**The msys2 base archive.** It is downloaded over HTTPS from
`mirror.msys2.org` and its sha256 is checked against a constant compiled into
the binary before a single file is unpacked. Upstream publishes no checksum
file next to the archive, so that constant is the whole of the verification:
whoever can change it, or can hand you a build of ProxSpace with a different
one in it, decides what lands on your disk. It is recomputed by hand and
cross-checked against a second mirror whenever the msys2 version is bumped, and
a mismatch aborts the install rather than warning about it.

**Packages.** Everything after the base archive comes from the msys2
repositories through `pacman`, which checks upstream's own signatures with the
msys2 keyring. ProxSpace does not disable that check anywhere, and the one
pinned package — `arm-none-eabi-binutils`, held back because a newer one breaks
the firmware build — is installed from a `repo.msys2.org` URL and verified the
same way. Mirror ranking (`proxspace mirrors rank`) reorders that list by speed
and can add nothing to it that is not already in the list msys2 shipped.

**Releases.** `proxspace.exe` is built by `release.yml` on GitHub-hosted
runners and published as a release artifact. It is not code-signed, so Windows
will warn about it, and there is no way to tell a real build from a forged one
except where you downloaded it: only the
[releases page of this repository](https://github.com/phntm-lab/ProxSpace/releases).
`proxspace update` asks GitHub whether a newer release exists and prints a
link — it never downloads or replaces anything by itself.

**The folder.** Everything ProxSpace creates lives next to the executable and
nothing is written to `%APPDATA%`, `%TEMP%` or the user profile. It never asks for
administrator rights, and a msys2 tree installed by one user is writable by
whoever can write to that folder — put it somewhere only you can write to if
that matters on your machine.

## What is out of scope

The toolchain ProxSpace installs is msys2's and the client it builds is
[proxmark3](https://github.com/RfidResearchGroup/proxmark3)'s. A vulnerability
in `gcc`, in Qt, in the proxmark3 client or in an msys2 package belongs to the
project that ships it. Report it here only if ProxSpace is what exposes it —
by pinning something dangerous, by weakening a check, or by installing it in a
way upstream does not intend.
