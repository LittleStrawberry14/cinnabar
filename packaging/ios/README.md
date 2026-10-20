# iOS (experimental)

This is the start of an iOS port of Cinnabar.

**Current status:** scaffolding only.  
It produces an **unsigned .ipa** that is **not a working game** yet.

### How to get the IPA
1. Go to the Actions tab on GitHub
2. Select the **iOS** workflow
3. Click **Run workflow**
4. Download the artifact called `cinnabar-ios-unsigned`

You will need a tool like Sideloadly / TrollStore / AltStore to install unsigned IPAs.

### What still needs to be done
- Real Bevy + Metal windowing on iOS
- Go core cross-compile
- Asset download + first-run flow
- Proper app lifecycle and signing setup

This will take more work. The structure is in place so we can keep building on it.
