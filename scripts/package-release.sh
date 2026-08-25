#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail

readonly program='mural'

usage() {
  cat >&2 <<'EOF'
Usage: scripts/package-release.sh vMAJOR.MINOR.PATCH TARGET

Environment:
  BINARY_DIR        Directory containing murald and muralctl
                    (default: target/release)
  OUTPUT_DIR        Artifact directory (default: dist)
                    Existing output files are never overwritten
  RELEASE_REF       Git object for the source archive (default: HEAD)
  SOURCE_DATE_EPOCH Archive timestamp (default: RELEASE_REF commit time)
EOF
}

if (( $# != 2 )); then
  usage
  exit 2
fi

readonly release_tag=$1
readonly target=$2

if [[ ! ${release_tag} =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
  echo "release tag must match vMAJOR.MINOR.PATCH: ${release_tag}" >&2
  exit 2
fi
readonly version=${BASH_REMATCH[1]}
if [[ ! ${target} =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  echo "invalid release target: ${target}" >&2
  exit 2
fi

readonly binary_dir=${BINARY_DIR:-target/release}
readonly output_dir=${OUTPUT_DIR:-dist}
readonly release_ref=${RELEASE_REF:-HEAD}
readonly archive_root="${program}-${version}-${target}"
readonly binary_archive="${archive_root}.tar.gz"
readonly source_root="${program}-${version}"
readonly source_archive="${program}-${version}-src.tar.gz"
readonly checksums='SHA256SUMS'

source_date_epoch=${SOURCE_DATE_EPOCH:-}
if [[ -z ${source_date_epoch} ]]; then
  source_date_epoch=$(git show -s --format=%ct "${release_ref}^{commit}")
fi
readonly source_date_epoch

if [[ ! ${source_date_epoch} =~ ^[0-9]+$ ]]; then
  echo 'SOURCE_DATE_EPOCH must be an integer' >&2
  exit 2
fi

workspace_version=$(
  cargo metadata --locked --no-deps --format-version 1 |
    python3 -c \
      'import json, sys; print(json.load(sys.stdin)["packages"][0]["version"])'
)
readonly workspace_version
if [[ ${workspace_version} != "${version}" ]]; then
  echo \
    "release tag ${release_tag} does not match workspace version ${workspace_version}" \
    >&2
  exit 1
fi

for executable in murald muralctl; do
  binary="${binary_dir}/${executable}"
  if [[ ! -x ${binary} ]]; then
    echo "release binary is missing or not executable: ${binary}" >&2
    exit 1
  fi
  if [[ $("${binary}" --version) != "${executable} ${version}" ]]; then
    echo "release binary has the wrong version: ${binary}" >&2
    exit 1
  fi
done

tmp_dir=$(mktemp -d)
readonly tmp_dir
trap 'rm -rf "${tmp_dir}"' EXIT

readonly package_dir="${tmp_dir}/${archive_root}"
install -Dm755 "${binary_dir}/murald" "${package_dir}/bin/murald"
install -Dm755 "${binary_dir}/muralctl" "${package_dir}/bin/muralctl"
install -Dm644 docs/man/mural.7 "${package_dir}/share/man/man7/mural.7"
install -Dm644 docs/man/murald.1 "${package_dir}/share/man/man1/murald.1"
install -Dm644 docs/man/muralctl.1 "${package_dir}/share/man/man1/muralctl.1"
install -Dm644 \
  docs/man/mural-config.5 \
  "${package_dir}/share/man/man5/mural-config.5"
install -Dm644 \
  examples/config \
  "${package_dir}/share/doc/mural/examples/config"
install -Dm644 \
  LICENSE-APACHE \
  "${package_dir}/share/licenses/mural/LICENSE-APACHE"
install -Dm644 LICENSE-MIT "${package_dir}/share/licenses/mural/LICENSE-MIT"
install -d "${package_dir}/share/systemd/user"
sed 's|@BINDIR@|%h/.local/bin|g' dist/systemd/murald.service.in \
  >"${package_dir}/share/systemd/user/murald.service"
chmod 0644 "${package_dir}/share/systemd/user/murald.service"

tar \
  --sort=name \
  --mtime="@${source_date_epoch}" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  -C "${tmp_dir}" \
  -cf - "${archive_root}" \
  | gzip -n >"${tmp_dir}/${binary_archive}"

git archive \
  --format=tar \
  --prefix="${source_root}/" \
  "${release_ref}" \
  | gzip -n >"${tmp_dir}/${source_archive}"

(
  cd "${tmp_dir}"
  sha256sum "${binary_archive}" "${source_archive}" >"${checksums}"
)

install -d "${output_dir}"
for artifact in "${binary_archive}" "${source_archive}" "${checksums}"; do
  destination="${output_dir}/${artifact}"
  if [[ -e ${destination} || -L ${destination} ]]; then
    echo "refusing to overwrite existing release artifact: ${destination}" >&2
    exit 1
  fi
done

install -m0644 "${tmp_dir}/${binary_archive}" "${output_dir}/${binary_archive}"
install -m0644 "${tmp_dir}/${source_archive}" "${output_dir}/${source_archive}"
install -m0644 "${tmp_dir}/${checksums}" "${output_dir}/${checksums}"

printf 'Created %s\n' \
  "${output_dir}/${binary_archive}" \
  "${output_dir}/${source_archive}" \
  "${output_dir}/${checksums}"
