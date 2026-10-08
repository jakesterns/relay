# The NDI(R) runtime that Relay's installer bundles (option B,
# docs/dev/ndi-licensing.md). NDI(R) is a registered trademark of Vizrt NDI AB.
#
# Read by scripts/fetch-ndi-runtime.ps1 and scripts/stage-bundle.ps1. Every
# file is checked against the SHA-256 here; a mismatch fails the build, it is
# never "updated" automatically.
#
# NDI's redistributable URL is not versioned: when NDI publishes a new
# runtime, the installer hash stops matching and the release build fails.
# That is deliberate. The NDI SDK licence (s2b) requires the bundled runtime
# to be the latest one, so the fix is to bump this file (see "Updating the
# bundled runtime" in docs/dev/ndi-licensing.md), not to skip the check.
@{
    # FileVersion of Processing.NDI.Lib.x64.dll (and of the installer).
    Version            = '6.3.2.0'
    # What http://ndi.link/NDIRedistV6 redirects to (checked 2026-10-07).
    InstallerUrl       = 'https://downloads.ndi.tv/SDK/NDI_SDK/NDI%206%20Runtime.exe'
    InstallerSha256    = '7ee73eedb56402bca5100868353dbab4e944b6c37d2d9881580698a3b61346cd'
    # Authenticode signer of both the installer and the DLL.
    Signer             = 'CN=Vizrt AG, O=Vizrt AG'

    # Files taken out of the installer (its "app\" folder) and shipped in
    # Relay's install folder, next to relay-share.exe. The licences file is
    # NDI's own notice: "This file should be included with all distribution
    # of the binary files included with the NDI SDK."
    Files              = @(
        @{ Name = 'Processing.NDI.Lib.x64.dll';      Sha256 = '2b6602075868ba4401f82f417d72424805d69b11ca86078023d0d489ff45dd84'; Signed = $true }
        @{ Name = 'Processing.NDI.Lib.Licenses.txt'; Sha256 = '9f18e17e95324b1907c4de45f676ce2fedbc0a5c4ff6ed451b31c3fcb1229b8d'; Signed = $false }
    )

    # The NDI runtime installer is Inno Setup. innoextract unpacks it without
    # running it, so the build machine is never changed (no install, no
    # NDI_RUNTIME_DIR_V6). zlib licence; https://constexpr.org/innoextract/
    InnoextractUrl     = 'https://github.com/dscharrer/innoextract/releases/download/1.9/innoextract-1.9-windows.zip'
    InnoextractSha256  = '6989342c9b026a00a72a38f23b62a8e6a22cc5de69805cf47d68ac2fec993065'
}
