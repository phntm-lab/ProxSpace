@echo off
title ProxSpace - flash bootrom
echo.
echo   Flashing bootrom.elf to a Proxmark3 on COM5.
echo.
echo   Read this before pressing a key. The bootrom is what lets the device be
echo   flashed at all: a bootrom that does not come up cannot be replaced over
echo   USB, and recovering from that needs a JTAG programmer. Flash it only
echo   when you have a reason to, and do not interrupt it.
echo.
echo   The client directory next to this file has to hold flasher.exe and
echo   bootrom.elf.
echo.
echo   The port is written into this file. Edit the flasher line below if your
echo   device is not on COM5.
echo.
pause

echo.
echo   Flashing, do not unplug the device...
echo.
call client\setup.bat
client\flasher.exe com5 -b client\bootrom.elf

echo.
pause
