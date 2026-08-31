@echo off
rem Start the client through the pm3 script this fork ships, from the directory
rem that script expects to be run from.
rem
rem The port is found on its own. To force one, put it on the bash line:
rem     bash pm3 -p COM3
cd "%~dp0client"
call setup.bat
bash pm3
pause
