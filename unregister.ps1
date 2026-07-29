# Remove everything register.ps1 created (current user only).
# NOTE: ASCII-only file on purpose - Windows PowerShell 5.1 reads unmarked files as ANSI.

$ErrorActionPreference = "SilentlyContinue"

$exts = @("jpg","jpeg","jpe","jfif","png","apng","gif","webp","bmp","dib","ico",
          "tif","tiff","tga","qoi","hdr","exr","pnm","pbm","pgm","ppm","dds","ff",
          "jxl","avif","avifs","heic","heif","hif",
          "cr2","crw","nef","nrw","arw","srf","sr2","dng","raf","orf","rw2","pef",
          "srw","erf","mrw","mos","iiq","3fr","dcr","kdc","mef","rwl","x3f")

foreach ($e in $exts) {
    Remove-Item -Path "HKCU:\Software\Classes\Zoetrope.AssocFile.$e" -Recurse -Force
}
Remove-Item -Path "HKCU:\Software\Classes\Applications\Zoetrope.exe" -Recurse -Force
Remove-Item -Path "HKCU:\Software\Zoetrope" -Recurse -Force
Remove-ItemProperty -Path "HKCU:\Software\RegisteredApplications" -Name "Zoetrope" -Force

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class ShellNotify {
    [DllImport("shell32.dll")] public static extern void SHChangeNotify(int eventId, uint flags, IntPtr item1, IntPtr item2);
}
"@
[ShellNotify]::SHChangeNotify(0x08000000, 0x1000, [IntPtr]::Zero, [IntPtr]::Zero)

Write-Output "OK: Zoetrope unregistered."
