@echo off
title ProxSpace - flash fullimage
echo.
echo   Flashing fullimage.elf to a Proxmark3 on COM5.
echo.
echo   The client directory next to this file has to hold flasher.exe and
echo   fullimage.elf. It does if this archive was packed by ProxSpace and the
echo   build it came from succeeded.
echo.
echo   The port is written into this file. Edit the flasher line below if your
echo   device is not on COM5.
echo.
pause

echo.
echo   Flashing, do not unplug the device...
echo.
call client\setup.bat
client\flasher.exe com5 -b client\fullimage.elf

echo.
pause
