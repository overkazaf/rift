# Homebrew cask template for Rift.
#
# Publish by copying this file to a tap, e.g. github.com/overkazaf/homebrew-tap
# as Casks/rift.rb, then:  brew install --cask overkazaf/tap/rift
#
# Per release: set `version` and `sha256` (from Rift-<version>-macos.dmg.sha256
# attached to the GitHub Release). Until the dmg is signed + notarized (see
# .github/workflows/release.yml TODOs) Gatekeeper will need a manual "Open anyway".
cask "rift" do
  version "0.4.0"
  sha256 "REPLACE_WITH_SHA256_OF_Rift-0.4.0-macos.dmg"

  url "https://github.com/overkazaf/rift/releases/download/v#{version}/Rift-#{version}-macos.dmg"
  name "Rift"
  desc "Cyberpunk terminal emulator with AI, SSH, time travel and developer tools"
  homepage "https://github.com/overkazaf/rift"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: :ventura

  app "Rift.app"
  binary "#{appdir}/Rift.app/Contents/MacOS/rift"

  zap trash: [
    "~/.config/rift",
    "~/Library/Saved Application State/com.overkazaf.rift.savedState",
  ]
end
