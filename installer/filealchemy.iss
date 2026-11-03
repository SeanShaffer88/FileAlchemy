; =============================================================================
;  FileAlchemy — Inno Setup 6 script  ->  FileAlchemy-v{VERSION}-x64-Setup.exe
;
;  Per-user install: no UAC prompt, no admin rights, no HKLM writes.
;  Files land in %LOCALAPPDATA%\Programs\FileAlchemy; user state lives in
;  %LOCALAPPDATA%\FileAlchemy (created by the app on first run).
;
;  Build:
;     iscc /DAppVersion=1.0.0 /DSourceRoot=..\dist\portable ^
;          /DOutputDir=..\dist installer\filealchemy.iss
;  (scripts/package.ps1 does this for you.)
; =============================================================================

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
#ifndef SourceRoot
  #define SourceRoot "..\dist\portable"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppName "FileAlchemy"
#define AppPublisher "Sean Shaffer"
#define AppExe "FileAlchemy.exe"
#define AppUrl "https://github.com/SeanShaffer88/FileAlchemy"

[Setup]
; A stable AppId keeps upgrades in place instead of stacking duplicates.
AppId={{6F2F1E9C-2C7B-4E4E-9A1B-5B0E8C7A1D31}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir={#OutputDir}
OutputBaseFilename=FileAlchemy-v{#AppVersion}-x64-Setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\{#AppExe}
LicenseFile=..\LICENSE
MinVersion=10.0.17763
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "shellmenu"; Description: "Add ""Convert with FileAlchemy"" to the Explorer right-click menu (no admin rights needed)"; GroupDescription: "Integration:"; Flags: checkedonce

[Files]
; The executable, the bundled engines and the licence. Deliberately NOT
; portable.dat and NOT data\ — an installed copy keeps its state per user.
Source: "{#SourceRoot}\FileAlchemy.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceRoot}\bin\*"; DestDir: "{app}\bin"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
Source: "{#SourceRoot}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--install-shell-menu"; Flags: runhidden waituntilterminated; Tasks: shellmenu; StatusMsg: "Adding the Explorer context menu..."
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; runhidden + waituntilterminated so the HKCU key is gone before the files are.
Filename: "{app}\{#AppExe}"; Parameters: "--uninstall-shell-menu"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveShellMenu"

[UninstallDelete]
; Only build leftovers. The user's own settings in %LOCALAPPDATA%\FileAlchemy
; and any converted files are intentionally preserved across uninstalls.
Type: filesandordirs; Name: "{app}\data\temp"
