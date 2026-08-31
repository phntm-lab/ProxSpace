@echo off
rem Environment for the client packed into this archive, sourced by the scripts
rem next to it. Qt is told where its platform plugin is, because the plugin
rem travels in libs\ rather than in a Qt installation on this machine; the
rem shell helpers this fork ships live one directory further in.
set "HOME=%~dp0"
set "QT_PLUGIN_PATH=%HOME%\libs\"
set "QT_QPA_PLATFORM_PLUGIN_PATH=%QT_PLUGIN_PATH%"
set "PATH=%QT_PLUGIN_PATH%;%QT_PLUGIN_PATH%shell\;%PATH%"
set MSYSTEM=UCRT64
