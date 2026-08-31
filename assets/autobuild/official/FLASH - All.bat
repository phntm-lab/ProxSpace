@echo off
title ProxSpace - flash bootrom and fullimage
echo.
echo   Flashing bootrom.elf and fullimage.elf to a Proxmark3 on COM5.
echo.
echo   The client directory next to this file has to hold flasher.exe,
echo   bootrom.elf and fullimage.elf. It does if this archive was packed by
echo   ProxSpace and the build it came from succeeded.
echo.
echo   The port is written into this file. Edit the flasher line below if your
echo   device is not on COM5.
echo.
pause

echo.
echo   Flashing, do not unplug the device...
echo.
call client\setup.bat
client\flasher.exe com5 -b client\bootrom.elf client\fullimage.elf

echo.
pause
