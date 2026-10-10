# version and sha256 below are rewritten by .github/workflows/release.yml on
# every tag; do not hand-edit them for releases.
cask "ferrite" do
  arch arm: "aarch64", intel: "x86_64"

  version "0.1.0"
  sha256 arm:   "REPLACE_WITH_AARCH64_SHA256_FROM_RELEASE_JOB",
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
