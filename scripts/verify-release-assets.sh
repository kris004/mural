#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: scripts/verify-release-assets.sh \
  vMAJOR.MINOR.PATCH TARGET ARTIFACT_DIR [RELEASE_REF]
EOF
}

if (( $# < 3 || $# > 4 )); then
  usage
  exit 2
fi

readonly release_tag=$1
readonly target=$2
readonly artifact_dir=$3
readonly release_ref=${4:-HEAD}

if [[ ! ${release_tag} =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
  echo "release tag must match vMAJOR.MINOR.PATCH: ${release_tag}" >&2
  exit 2
fi
readonly version=${BASH_REMATCH[1]}
if [[ ! ${target} =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  echo "invalid release target: ${target}" >&2
  exit 2
fi

readonly archive_root="mural-${version}-${target}"
readonly binary_archive="${archive_root}.tar.gz"
readonly source_root="mural-${version}"
readonly source_archive="mural-${version}-src.tar.gz"

for artifact in "${binary_archive}" "${source_archive}" SHA256SUMS; do
  if [[ ! -f ${artifact_dir}/${artifact} ]]; then
    echo "release artifact is missing: ${artifact_dir}/${artifact}" >&2
    exit 1
  fi
done

actual_files=$(
  find "${artifact_dir}" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort
)
expected_files=$(printf '%s\n' SHA256SUMS "${binary_archive}" "${source_archive}")
if [[ ${actual_files} != "${expected_files}" ]]; then
  echo 'artifact directory contains an unexpected file set' >&2
  diff -u <(printf '%s\n' "${expected_files}") <(printf '%s\n' "${actual_files}") \
    >&2 || true
  exit 1
fi

(
  cd "${artifact_dir}"
  sha256sum --check SHA256SUMS
)

tmp_dir=$(mktemp -d)
readonly tmp_dir
trap 'rm -rf "${tmp_dir}"' EXIT
mkdir "${tmp_dir}/binary" "${tmp_dir}/source" "${tmp_dir}/expected-source"
tar -xzf "${artifact_dir}/${binary_archive}" -C "${tmp_dir}/binary"
tar -xzf "${artifact_dir}/${source_archive}" -C "${tmp_dir}/source"
git archive \
  --format=tar \
  --prefix="${source_root}/" \
  "${release_ref}" \
  | tar -xf - -C "${tmp_dir}/expected-source"
diff -qr \
  "${tmp_dir}/source/${source_root}" \
  "${tmp_dir}/expected-source/${source_root}"

readonly package_dir="${tmp_dir}/binary/${archive_root}"
cat >"${tmp_dir}/binary.expected" <<'EOF'
bin/muralctl
bin/murald
share/doc/mural/examples/config
share/licenses/mural/LICENSE-APACHE
share/licenses/mural/LICENSE-MIT
share/man/man1/muralctl.1
share/man/man1/murald.1
share/man/man5/mural-config.5
share/man/man7/mural.7
share/systemd/user/murald.service
EOF
find "${package_dir}" -type f -printf '%P\n' | LC_ALL=C sort \
  >"${tmp_dir}/binary.actual"
diff -u "${tmp_dir}/binary.expected" "${tmp_dir}/binary.actual"

for executable in murald muralctl; do
  binary="${package_dir}/bin/${executable}"
  [[ $(stat -c %a "${binary}") == 755 ]]
  [[ $("${binary}" --version) == "${executable} ${version}" ]]
  file "${binary}" | grep -Eq 'ELF 64-bit LSB pie executable, x86-64'
  if readelf -d "${binary}" | grep -Eq '\((RPATH|RUNPATH)\)'; then
    echo "release binary contains an RPATH or RUNPATH: ${binary}" >&2
    exit 1
  fi
  if ldd "${binary}" | grep -F 'not found'; then
    echo "release binary has an unresolved shared library: ${binary}" >&2
    exit 1
  fi
done

grep -Fx \
  'ExecStart=%h/.local/bin/murald' \
  "${package_dir}/share/systemd/user/murald.service"

while IFS= read -r file_path; do
  [[ $(stat -c %a "${package_dir}/share/${file_path}") == 644 ]]
done < <(find "${package_dir}/share" -type f -printf '%P\n')

printf 'Verified release assets for %s (%s) from %s.\n' \
  "${release_tag}" "${target}" "${release_ref}"
