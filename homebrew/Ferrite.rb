cask "ferrite" do
  version "0.1.0"
  sha256 "REPLACE_WITH_SHA256_FROM_SHA256SUMS"

  url "https://github.com/anomalyco/opencode/releases/download/v#{version}/Ferrite-#{version}.dmg",
       verified: "github.com/anomalyco/opencode/"
  name "Ferrite"
  desc "Sovereign Browser — Usability by Default, Privacy by Choice"
  homepage "https://anomalyco.github.io/opencode/"

  auto_updates true

  app "Ferrite.app"

  zap trash: [
    "~/Library/Application Support/ferrite",
    "~/Library/Preferences/com.ferrite.Browser.plist",
  ]
end
