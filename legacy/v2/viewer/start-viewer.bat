@echo off
chcp 65001 > nul
title ANTmemory Database Viewer
cd /d "%~dp0"
echo Starting ANTmemory Database Viewer...
node server.mjs
pause
