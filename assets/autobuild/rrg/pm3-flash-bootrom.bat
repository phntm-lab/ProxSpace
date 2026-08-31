@echo off
rem Flash the bootrom through the script this fork ships, from the directory
rem that script expects to be run from. The bootrom is what lets the device be
rem flashed at all, so do not interrupt it once it starts.
rem
rem The port is found on its own. To force one, put it on the bash line:
rem     bash pm3-flash-bootrom COM3
cd "%~dp0client"
call setup.bat
bash pm3-flash-bootrom
pause
