#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 6 ]]; then
  echo "Usage: $0 <tap-dir> <version> <arm-url> <arm-sha> <intel-url> <intel-sha>" >&2
  exit 1
fi

TAP_DIR="$1"
VERSION="$2"
ARM_URL="$3"
ARM_SHA="$4"
INTEL_URL="$5"
INTEL_SHA="$6"

FORMULA_PATH="${TAP_DIR}/Formula/tp7.rb"

mkdir -p "$(dirname "${FORMULA_PATH}")"

cat >"${FORMULA_PATH}" <<EOF
class Tp7 < Formula
  desc "Command-line file access for Teenage Engineering TP-7 field recorders"
  homepage "https://github.com/totocaster/tp7"
  version "${VERSION}"
  license "MIT"

  on_macos do
    on_arm do
      url "${ARM_URL}"
      sha256 "${ARM_SHA}"
    end

    on_intel do
      url "${INTEL_URL}"
      sha256 "${INTEL_SHA}"
    end
  end

  def install
    bin.install "bin/tp7"
  end

  test do
    version_output = shell_output("#{bin}/tp7 --version")
    assert_match "tp7 #{version}", version_output

    help_output = shell_output("#{bin}/tp7 --help")
    assert_match "Teenage Engineering TP-7 file access CLI", help_output
  end
end
EOF
