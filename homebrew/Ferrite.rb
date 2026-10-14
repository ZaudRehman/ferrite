# version and sha256 below are rewritten by .github/workflows/release.yml on
# every tag; do not hand-edit them for releases.
cask "ferrite" do
  arch arm: "aarch64", intel: "bf4cbc84782625b23913e794047fd57e13a1d6e37b96b642fb9dbaa9f1c164b5"

  version "0.1.0"
  sha256 arm:   "84b9360cf6eb97052edd078bb1c2a35769978ba24b389fd17ce5143e645ce53b",
         intel: "REPLACE_WITH_X86_64_SHA256_FROM_RELEASE_JOB"

  url "https://github.com/ZaudRehman/ferrite/releases/download/v#{version}/Ferrite-#{arch}-apple-darwin.dmg",
      verified: "github.com/ZaudRehman/ferrite/"
  name "Ferrite"
  desc "Privacy-first desktop browser"
  homepage "https://github.com/ZaudRehman/ferrite"

  depends_on macos: ">= :ventura"

  app "Ferrite.app"

  livecheck do
    url :stable
    strategy :github_latest
  end

  zap trash: [
    "~/Library/Application Support/Ferrite",
    "~/Library/Preferences/com.ferrite.Browser.plist",
  ]
end
