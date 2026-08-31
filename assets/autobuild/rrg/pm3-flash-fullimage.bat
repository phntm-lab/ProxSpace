@echo off
rem Flash the fullimage through the script this fork ships, from the directory
rem that script expects to be run from.
rem
rem The port is found on its own. To force one, put it on the bash line:
rem     bash pm3-flash-fullimage COM3
cd "%~dp0client"
call setup.bat
bash pm3-flash-fullimage
pause
