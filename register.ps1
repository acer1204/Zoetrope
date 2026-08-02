# Register Zoetrope as an image-viewer candidate for the current user (no admin needed).
# After running this, Zoetrope appears in:
#   - Settings > Apps > Default apps            (set defaults per file type there)
#   - Right-click an image > Open with          (check "Always" to set default quickly)
# Re-run this script if Zoetrope.exe is moved. Run unregister.ps1 to undo everything.
# NOTE: ASCII-only file on purpose - Windows PowerShell 5.1 reads unmarked files as ANSI.

$ErrorActionPreference = "Stop"

$exe = Join-Path $PSScriptRoot "Zoetrope.exe"
if (-not (Test-Path $exe)) { throw "Zoetrope.exe not found next to this script: $exe" }

$exts = @("jpg","jpeg","jpe","jfif","png","apng","gif","webp","bmp","dib","ico",
          "tif","tiff","tga","qoi","hdr","exr","pnm","pbm","pgm","ppm","dds","ff",
          # modern container formats
          "jxl","avif","avifs","heic","heif","hif",
          # camera RAW
          "cr2","cr3","crw","nef","nrw","arw","srf","sr2","dng","raf","orf","rw2","pef",
          "srw","erf","mrw","mos","iiq","3fr","dcr","kdc","mef","rwl","x3f")

$classes = "HKCU:\Software\Classes"
$caps    = "HKCU:\Software\Zoetrope\Capabilities"
$cmd     = '"' + $exe + '" "%1"'

# 1) Application capabilities (what makes it show up in Settings > Default apps)
New-Item -Path "$caps\FileAssociations" -Force | Out-Null
Set-ItemProperty -Path $caps -Name "ApplicationName" -Value "Zoetrope"
Set-ItemProperty -Path $caps -Name "ApplicationDescription" -Value "Fast GPU image viewer - smooth animation streaming, JPEG XL / AVIF / HEIC / RAW"

# 2) One ProgID per extension
foreach ($e in $exts) {
    $progid = "Zoetrope.AssocFile.$e"
    New-Item -Path "$classes\$progid\DefaultIcon" -Force | Out-Null
    New-Item -Path "$classes\$progid\shell\open\command" -Force | Out-Null
    Set-ItemProperty -Path "$classes\$progid" -Name "(Default)" -Value ("Zoetrope " + $e.ToUpper() + " File")
    Set-ItemProperty -Path "$classes\$progid\DefaultIcon" -Name "(Default)" -Value "$exe,0"
    Set-ItemProperty -Path "$classes\$progid\shell\open\command" -Name "(Default)" -Value $cmd
    Set-ItemProperty -Path "$caps\FileAssociations" -Name ".$e" -Value $progid
}

# 3) Register the application
New-Item -Path "HKCU:\Software\RegisteredApplications" -Force | Out-Null
Set-ItemProperty -Path "HKCU:\Software\RegisteredApplications" -Name "Zoetrope" -Value "Software\Zoetrope\Capabilities"

# 4) "Open with" entry
New-Item -Path "$classes\Applications\Zoetrope.exe\shell\open\command" -Force | Out-Null
New-Item -Path "$classes\Applications\Zoetrope.exe\SupportedTypes" -Force | Out-Null
Set-ItemProperty -Path "$classes\Applications\Zoetrope.exe" -Name "FriendlyAppName" -Value "Zoetrope"
Set-ItemProperty -Path "$classes\Applications\Zoetrope.exe\shell\open\command" -Name "(Default)" -Value $cmd
foreach ($e in $exts) {
    Set-ItemProperty -Path "$classes\Applications\Zoetrope.exe\SupportedTypes" -Name ".$e" -Value ""
}

# 5) Tell Explorer that associations changed
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class ShellNotify {
    [DllImport("shell32.dll")] public static extern void SHChangeNotify(int eventId, uint flags, IntPtr item1, IntPtr item2);
}
"@
[ShellNotify]::SHChangeNotify(0x08000000, 0x1000, [IntPtr]::Zero, [IntPtr]::Zero)  # SHCNE_ASSOCCHANGED

Write-Output "OK: Zoetrope registered for current user."
Write-Output "Exe: $exe"
Write-Output "Next: Settings > Apps > Default apps > search 'Zoetrope', or right-click an image > Open with."
