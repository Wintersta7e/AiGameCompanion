#!/usr/bin/env bash
# Makes the release assets from release/ (what scripts/build.sh writes) and
# checks them.
#
#   scripts/release-assets.sh <version> <out-dir>
#   scripts/release-assets.sh check <version> <out-dir>
#   scripts/release-assets.sh version
#
# The first form writes four files into <out-dir>, which must not exist: the
# zip of launcher.exe and config.example.toml (flat), a CycloneDX SBOM of the
# launcher's Rust dependencies for the Windows target the exe is built for
# (the host target would list Linux-only crates the exe does not contain), a
# CycloneDX SBOM of its npm runtime dependencies, and SHA256SUMS over the
# three. On any failure the directory is removed, so it is either absent or
# complete. check verifies SHA256SUMS, compares the zip's members with
# release/, reads both SBOMs and runs scripts/binary-audit.sh on the zip and
# the SBOMs. version prints the launcher crate's version.
set -euo pipefail
# A glob matches dotfiles too and expands to nothing when nothing matches.
shopt -s nullglob dotglob

TARGET=x86_64-pc-windows-msvc
MANIFEST=crates/launcher/src-tauri/Cargo.toml
# Where cargo cyclonedx writes the Rust SBOM; moved into <out-dir>.
CARGO_SBOM=crates/launcher/src-tauri/launcher.cdx.json
SEMVER='^[0-9]+\.[0-9]+\.[0-9]+$'

OUT_DIR=''
UNPACKED=''
COMPLETE=no
FAILURES=0

usage() {
	printf 'usage: release-assets.sh <version> <out-dir> | check <version> <out-dir> | version\n' >&2
	exit 2
}

fail() {
	printf 'release assets: FAIL %s\n' "$1" >&2
	exit 1
}

check_fail() {
	printf 'release assets check: FAIL %s\n' "$1"
	FAILURES=$((FAILURES + 1))
}

# The crate's version as cargo resolves it. --locked also stops a lock file
# that needs an update: cargo cyclonedx reads the graph without it.
crate_version() {
	local metadata
	metadata=$(cargo metadata --format-version 1 --locked --filter-platform "${TARGET}" \
		--manifest-path "${MANIFEST}") || return 1
	jq -r '. as $m | .packages[] | select(.id == $m.workspace_members[0]) | .version' <<<"${metadata}"
}

asset_names() {
	ZIP="AiGameCompanion-v$1-win64.zip"
	RUST_SBOM="AiGameCompanion-v$1-rust.cdx.json"
	NPM_SBOM="AiGameCompanion-v$1-npm.cdx.json"
}

remove_partial() {
	rm -f -- "${CARGO_SBOM}"
	if [[ "${COMPLETE}" != yes ]]; then rm -rf -- "${OUT_DIR}"; fi
}

make_assets() {
	local version=$1 out=$2 crate rust_n npm_n
	local -a files=()
	[[ "${version}" =~ ${SEMVER} ]] || fail "version '${version}' is not X.Y.Z"
	[[ -f release/launcher.exe && -f release/config.example.toml ]] ||
		fail 'release/ lacks launcher.exe or config.example.toml; run scripts/build.sh first'
	crate=$(crate_version)
	[[ "${crate}" == "${version}" ]] || fail "version ${version} is not the launcher crate's ${crate}"
	[[ ! -e "${out}" ]] || fail "the out dir $2 already exists"
	[[ ! -e "${CARGO_SBOM}" ]] || fail "${CARGO_SBOM} already exists"
	mkdir -- "${out}" || fail "the out dir $2 cannot be created"
	OUT_DIR=${out}
	trap remove_partial EXIT
	asset_names "${version}"

	zip -q -j -X "${out}/${ZIP}" release/launcher.exe release/config.example.toml
	cargo cyclonedx --format json --target "${TARGET}" --manifest-path "${MANIFEST}"
	# The launcher's own entry names its source directory by absolute path:
	# the build host's checkout path would be published.
	jq --arg from "file://${ROOT}/" --arg to 'file://./' \
		'walk(if type == "string" then split($from) | join($to) else . end)' \
		"${CARGO_SBOM}" >"${out}/${RUST_SBOM}"
	rm -f -- "${CARGO_SBOM}"
	(cd crates/launcher && npm sbom --sbom-format cyclonedx --omit dev) >"${out}/${NPM_SBOM}"
	if grep -qF -- "${ROOT}" "${out}/${RUST_SBOM}" "${out}/${NPM_SBOM}"; then
		fail 'an SBOM holds the path of the checkout'
	fi
	rust_n=$(jq '.components | length' "${out}/${RUST_SBOM}")
	npm_n=$(jq '.components | length' "${out}/${NPM_SBOM}")
	(cd -- "${out}" && sha256sum -- "${ZIP}" "${RUST_SBOM}" "${NPM_SBOM}" >SHA256SUMS)
	files=("${out}"/*)
	printf 'release assets: files %s rust components %s npm components %s\n' "${#files[@]}" "${rust_n}" "${npm_n}"
	[[ "${rust_n}" -gt 0 && "${npm_n}" -gt 0 ]] || fail 'an SBOM lists no component'
	COMPLETE=yes
}

remove_unpacked() {
	rm -rf -- "${UNPACKED}"
}

# sbom_components <file>: the SBOM's component count, 0 when it is unreadable.
sbom_components() {
	local n
	if n=$(jq -e '.components | length' "$1" 2>/dev/null); then printf '%s\n' "${n}"; else printf '0\n'; fi
}

check_assets() {
	local version=$1 out=$2 name listed expected members rust_n=0 npm_n=0
	local -a files=()
	[[ "${version}" =~ ${SEMVER} ]] || fail "version '${version}' is not X.Y.Z"
	[[ -d "${out}" ]] || fail "the out dir $2 does not exist"
	asset_names "${version}"
	files=("${out}"/*)
	[[ "${#files[@]}" -eq 4 ]] || check_fail "the out dir holds ${#files[@]} entries, not 4"
	for name in "${ZIP}" "${RUST_SBOM}" "${NPM_SBOM}" SHA256SUMS; do
		[[ -f "${out}/${name}" ]] || check_fail "${name} is missing"
	done

	if [[ -f "${out}/SHA256SUMS" ]]; then
		listed=$(sed -E 's/^[0-9a-f]{64}  //' "${out}/SHA256SUMS" | LC_ALL=C sort | paste -sd ' ')
		expected=$(printf '%s\n' "${ZIP}" "${RUST_SBOM}" "${NPM_SBOM}" | LC_ALL=C sort | paste -sd ' ')
		[[ "${listed}" == "${expected}" ]] || check_fail "SHA256SUMS lists '${listed}', not '${expected}'"
		(cd -- "${out}" && sha256sum -c --strict SHA256SUMS) || check_fail 'SHA256SUMS does not verify'
	fi

	if [[ -f "${out}/${ZIP}" ]]; then
		members=$(unzip -Z1 "${out}/${ZIP}" | LC_ALL=C sort | paste -sd ' ') || members=''
		[[ "${members}" == 'config.example.toml launcher.exe' ]] ||
			check_fail "the zip holds '${members}', not 'config.example.toml launcher.exe'"
		UNPACKED=$(mktemp -d)
		trap remove_unpacked EXIT
		unzip -q "${out}/${ZIP}" -d "${UNPACKED}" || check_fail 'the zip cannot be extracted'
		for name in launcher.exe config.example.toml; do
			cmp -s -- "${UNPACKED}/${name}" "release/${name}" || check_fail "the zip's ${name} differs from release/${name}"
		done
	fi

	if [[ -f "${out}/${RUST_SBOM}" ]]; then
		rust_n=$(sbom_components "${out}/${RUST_SBOM}")
		[[ "${rust_n}" -gt 0 ]] || check_fail "${RUST_SBOM} lists no component"
		jq -e 'any(.components[]?; .name == "tauri")' "${out}/${RUST_SBOM}" >/dev/null ||
			check_fail "${RUST_SBOM} does not list tauri"
		jq -e 'any(.components[]?; .name == "zbus") | not' "${out}/${RUST_SBOM}" >/dev/null ||
			check_fail "${RUST_SBOM} lists zbus, a crate of the Linux host graph"
	fi
	if [[ -f "${out}/${NPM_SBOM}" ]]; then
		npm_n=$(sbom_components "${out}/${NPM_SBOM}")
		[[ "${npm_n}" -gt 0 ]] || check_fail "${NPM_SBOM} lists no component"
	fi

	if [[ -f "${out}/${ZIP}" && -f "${out}/${RUST_SBOM}" && -f "${out}/${NPM_SBOM}" ]]; then
		scripts/binary-audit.sh audit --public-runner --expected-version "${version}" \
			"${out}/${ZIP}" "${out}/${RUST_SBOM}" "${out}/${NPM_SBOM}" || check_fail 'the binary audit failed'
	fi

	printf 'release assets check: files %s rust components %s npm components %s failures %s\n' \
		"${#files[@]}" "${rust_n}" "${npm_n}" "${FAILURES}"
	[[ "${FAILURES}" -eq 0 ]]
}

main() {
	local mode=${1:-} out=''
	case "${mode}" in
	version) [[ $# -eq 1 ]] || usage ;;
	check)
		[[ $# -eq 3 ]] || usage
		out=$(realpath -m -- "$3")
		;;
	*)
		[[ $# -eq 2 ]] || usage
		out=$(realpath -m -- "$2")
		;;
	esac
	cd -- "$(dirname -- "$0")/.." || exit 1
	ROOT=$(pwd -P)
	case "${mode}" in
	version) crate_version ;;
	check) check_assets "$2" "${out}" ;;
	*) make_assets "$1" "${out}" ;;
	esac
}

main "$@"
