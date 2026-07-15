Set WshShell = CreateObject("WScript.Shell")
root = CreateObject("Scripting.FileSystemObject").GetParentFolderName(WScript.ScriptFullName)
WshShell.CurrentDirectory = root
WshShell.Run "powershell.exe -NoProfile -ExecutionPolicy Bypass -File """ & root & "\scripts\restart-desktop.ps1""", 0, False
