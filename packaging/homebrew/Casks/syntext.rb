cask "syntext" do
  arch arm: "arm64", intel: "x86_64"

  version "__VERSION__"
  sha256 arm:   "__ARM_SHA256__",
         intel: "__X86_SHA256__"

  url "https://github.com/whit3rabbit/syntext/releases/download/v#{version}/st-#{version}-macos-#{arch}.zip"
  name "syntext"
  desc "Hybrid code search index for agent workflows"
  homepage "https://github.com/whit3rabbit/syntext"

  binary "st"
end
