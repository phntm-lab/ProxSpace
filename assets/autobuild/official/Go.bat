@echo off
rem Start the proxmark3 client of this archive on COM5.
rem Put your own port on the last line if the device is on another one.
call client\setup.bat
client\proxmark3 COM5
