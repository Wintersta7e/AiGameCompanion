#!/usr/bin/env bash
# Counts build-host path shapes and key shapes in release files, in their
# ASCII and UTF-16LE strings, and checks the exe's PE hardening flags.
#
#   scripts/binary-audit.sh audit [--public-runner] [--expected-version <v>] <dir|zip|file>...
#   scripts/binary-audit.sh self-test
#   scripts/binary-audit.sh shapes
#   scripts/binary-audit.sh identity < records
#
# audit reads every regular file of each input (a zip is extracted to a
# temporary directory outside the repository) and prints, per file, one count
# per shape, never the matched text. Identity strings come only from the
# comma-separated CI_CHECK_IDENTITY variable and are never printed; without it
# the audit fails unless --public-runner is given. --expected-version makes
# every PE's ProductVersion equal to it. Every file is processed and every line
# printed before the exit status says whether anything failed. identity
# matches the same entries against text records on stdin and prints the
# location and entry number of each hit; without entries it matches nothing.
set -uo pipefail

# The shape list: label, class, case (i = case-insensitive), scope, ERE.
# `binary` rows are audited in release files only; `binary+text` rows are
# also meant for tracked text. No row has a word-boundary anchor: compiled
# string literals are packed with no separator.
shapes_table() {
	cat <<'SHAPES'
path-home	path	i	binary+text	/home/[^/[:space:]]
path-root	path	i	binary+text	/root/
path-users	path	cs	binary+text	/Users/[^/[:space:]]
path-mnt-drive	path	i	binary+text	/mnt/[a-z]/
path-tmp	path	i	binary	/tmp/
path-drive-users	path	i	binary+text	[a-z]:(\\|/)+users(\\|/)
path-drive-a	path	i	binary	[a-z]:(\\|/)+a(\\|/)
path-wsl-unc	path	i	binary+text	(\\|/){2}wsl(\.localhost|\$)(\\|/)
key-aiza	key	cs	binary	AIza[0-9A-Za-z_-]{35}
key-sk-typed	key	cs	binary	sk-(proj|svcacct|admin|ant)-[A-Za-z0-9_-]{20,}
key-sk-legacy	key	cs	binary	sk-[A-Za-z0-9]{40,}
key-gh-token	key	cs	binary	gh[pousr]_[A-Za-z0-9]{36}
key-gh-pat	key	cs	binary	github_pat_[A-Za-z0-9_]{22,}
key-akia	key	cs	binary	AKIA[0-9A-Z]{16}
key-pem-private	key	cs	binary	-----BEGIN ([A-Z]+ )?PRIVATE KEY
SHAPES
}

# The external tools audit mode runs (llvm-readobj and unzip only when needed).
AUDIT_TOOLS=(strings grep sha256sum wc find sort mktemp rm)

load_shapes() {
	local label case_ ere
	SHAPE_LABEL=()
	SHAPE_CASE=()
	SHAPE_ERE=()
	while IFS=$'\t' read -r label _ case_ _ ere; do
		SHAPE_LABEL+=("${label}")
		SHAPE_CASE+=("${case_}")
		SHAPE_ERE+=("${ere}")
	done < <(shapes_table || true)
}

# Collects every temporary directory for removal on exit.
TEMP_DIRS=()
cleanup() { if [[ "${#TEMP_DIRS[@]}" -gt 0 ]]; then rm -rf -- "${TEMP_DIRS[@]}"; fi; }
trap cleanup EXIT

# new_temp_dir <var>: makes a temporary directory, stores its path in <var>
# and registers it for removal on exit. Called in this shell, never in
# $(...): a subshell would lose the registration and leave the directory.
new_temp_dir() {
	local temp_path
	temp_path=$(mktemp -d) || return 1
	TEMP_DIRS+=("${temp_path}")
	printf -v "$1" '%s' "${temp_path}"
}

# scan_file <file> <work dir>: sets STR_ASCII, STR_UTF16 (string counts) and
# SCAN_COUNT[i] (matching lines of both encodings for shape i). The audit and
# the self-test both call it.
scan_file() {
	local file=$1 work=$2 i n total flag
	local -a counts=()
	strings -a -n 6 -- "${file}" >"${work}/ascii" 2>/dev/null
	strings -a -n 6 -e l -- "${file}" >"${work}/utf16" 2>/dev/null
	STR_ASCII=$(wc -l <"${work}/ascii")
	STR_UTF16=$(wc -l <"${work}/utf16")
	SCAN_COUNT=()
	for i in "${!SHAPE_LABEL[@]}"; do
		flag=-E
		if [[ "${SHAPE_CASE[i]}" == i ]]; then flag=-Ei; fi
		mapfile -t counts < <(grep -c -h "${flag}" -e "${SHAPE_ERE[i]}" -- "${work}/ascii" "${work}/utf16" || true)
		total=0
		for n in "${counts[@]}"; do total=$((total + n)); done
		SCAN_COUNT[i]=${total}
	done
}

# Counts identity entries in both encodings; the entries reach grep through a
# descriptor fed by printf, never an argument or a file.
identity_count() {
	local work=$1 n total=0
	local -a counts=()
	mapfile -t counts < <(grep -F -i -c -h -f /dev/fd/3 -- "${work}/ascii" "${work}/utf16" 3< <(printf '%s\n' "${ID_ENTRIES[@]}") || true)
	for n in "${counts[@]}"; do total=$((total + n)); done
	printf '%s\n' "${total}"
}

# pe_check <file-headers view> <load-config view> <debug-directory view>:
# prints one "pe <property>: present|missing" line per property and sets
# PE_MISSING_NOW to the missing count. Every test reads a named line, so a
# changed output format reads as missing, never as present.
pe_check() {
	local headers=$1 load=$2 debug=$3 cookie pdb guard_count
	PE_MISSING_NOW=0
	pe_line machine "${headers}" '^[[:space:]]*Machine: IMAGE_FILE_MACHINE_AMD64( |$)'
	pe_line large-address-aware "${headers}" '^[[:space:]]*IMAGE_FILE_LARGE_ADDRESS_AWARE( |$)'
	if grep -q -E '^[[:space:]]*IMAGE_DLL_CHARACTERISTICS_DYNAMIC_BASE( |$)' <<<"${headers}"; then
		pe_line aslr "${headers}" '^[[:space:]]*IMAGE_DLL_CHARACTERISTICS_HIGH_ENTROPY_VA( |$)'
	else
		pe_result aslr missing
	fi
	pe_line dep "${headers}" '^[[:space:]]*IMAGE_DLL_CHARACTERISTICS_NX_COMPAT( |$)'
	pe_line gui-subsystem "${headers}" '^[[:space:]]*Subsystem: IMAGE_SUBSYSTEM_WINDOWS_GUI( |$)'
	# Control Flow Guard and CET compatibility, set by build.sh. Only Rust code
	# compiled with the flag is instrumented: the precompiled std and the C
	# dependencies are not, so a count above 0 is all this can assert.
	pe_line control-flow-guard "${headers}" '^[[:space:]]*IMAGE_DLL_CHARACTERISTICS_GUARD_CF( |$)'
	guard_count=$(grep -m 1 -E '^[[:space:]]*GuardCFFunctionCount:' <<<"${load}")
	guard_count=${guard_count#*GuardCFFunctionCount:}
	guard_count=${guard_count// /}
	if grep -q -E '^[[:space:]]*CF_INSTRUMENTED( |$)' <<<"${load}" && [[ "${guard_count}" =~ ^[0-9]+$ && "${guard_count}" -gt 0 ]]; then
		pe_result control-flow-guard-table present
	else
		pe_result control-flow-guard-table missing
	fi
	pe_line cet-compatible "${debug}" '^[[:space:]]*IMAGE_DLL_CHARACTERISTICS_EX_CET_COMPAT( |$)'
	cookie=$(grep -m 1 -E '^[[:space:]]*SecurityCookie:' <<<"${load}")
	cookie=${cookie#*SecurityCookie:}
	cookie=${cookie// /}
	if [[ -n "${cookie}" && "${cookie}" != 0x0 ]]; then pe_result stack-cookie present; else pe_result stack-cookie missing; fi
	pdb=$(grep -m 1 -E '^[[:space:]]*PDBFileName:' <<<"${debug}")
	pdb=${pdb#*PDBFileName:}
	pdb=${pdb// /}
	if [[ -n "${pdb}" && "${pdb}" != */* && "${pdb}" != *\\* ]]; then
		pe_result pdb-name-without-path present
	else
		pe_result pdb-name-without-path missing
	fi
}

pe_line() {
	if grep -q -E "$3" <<<"$2"; then pe_result "$1" present; else pe_result "$1" missing; fi
}

pe_result() {
	printf '  pe %s: %s\n' "$1" "$2"
	if [[ "$2" == missing ]]; then
		PE_MISSING_NOW=$((PE_MISSING_NOW + 1))
		PE_MISSING_NAMES+=("$1")
	fi
}

fail_line() { FAILURES+=("audit: FAIL $*"); }

need_tool() {
	command -v -- "$1" >/dev/null 2>&1 && return 0
	if [[ " ${MISSING_TOOLS} " != *" $1 "* ]]; then
		MISSING_TOOLS+=" $1"
		fail_line "missing tool $1"
	fi
	return 1
}

audit_pe() {
	local file=$1 label=$2 headers load debug version=''
	local -a lines=()
	PE_FILES=$((PE_FILES + 1))
	if [[ "${STR_ASCII}" -eq 0 || "${STR_UTF16}" -eq 0 ]]; then fail_line "${label} strings ${STR_ASCII}/${STR_UTF16}"; fi
	need_tool llvm-readobj || return 0
	if ! headers=$(llvm-readobj --file-headers "${file}" 2>/dev/null) ||
		! load=$(llvm-readobj --coff-load-config "${file}" 2>/dev/null) ||
		! debug=$(llvm-readobj --coff-debug-directory "${file}" 2>/dev/null); then
		fail_line "${label} llvm-readobj could not read it"
		return 0
	fi
	PE_MISSING_NAMES=()
	pe_check "${headers}" "${load}" "${debug}"
	PE_MISSING=$((PE_MISSING + PE_MISSING_NOW))
	local name
	for name in "${PE_MISSING_NAMES[@]}"; do fail_line "${label} pe-${name} missing"; done
	mapfile -t lines < <(strings -a -e l -- "${file}" | grep -A 1 -x -F ProductVersion || true)
	version=${lines[1]:-}
	printf '  ProductVersion: %s\n' "${version:-missing}"
	if [[ -n "${EXPECTED_VERSION}" && "${version}" != "${EXPECTED_VERSION}" ]]; then
		fail_line "${label} ProductVersion ${version:-missing}"
	fi
}

audit_file() {
	local file=$1 label=$2 work bytes sum i n magic=''
	new_temp_dir work || return 1
	bytes=$(wc -c <"${file}")
	sum=$(sha256sum <"${file}")
	scan_file "${file}" "${work}"
	FILES=$((FILES + 1))
	printf 'file %s bytes %s sha256 %s strings %s/%s\n' "${label}" "${bytes// /}" "${sum%% *}" "${STR_ASCII}" "${STR_UTF16}"
	for i in "${!SHAPE_LABEL[@]}"; do
		n=${SCAN_COUNT[i]}
		printf '  %s -> %s\n' "${SHAPE_LABEL[i]}" "${n}"
		if [[ "${n}" -gt 0 ]]; then
			HITS=$((HITS + n))
			fail_line "${label} ${SHAPE_LABEL[i]} ${n}"
		fi
	done
	if [[ "${#ID_ENTRIES[@]}" -gt 0 ]]; then
		n=$(identity_count "${work}")
		printf '  identity -> %s\n' "${n}"
		if [[ "${n}" -gt 0 ]]; then
			HITS=$((HITS + n))
			fail_line "${label} identity ${n}"
		fi
	fi
	IFS= LC_ALL=C read -r -n 2 magic <"${file}" || true
	if [[ "${magic}" == MZ ]]; then audit_pe "${file}" "${label}"; fi
	rm -rf -- "${work}"
}

# audit_tree <directory> <label prefix>: every regular file, sorted.
audit_tree() {
	local dir=$1 prefix=$2 file n=0
	local -a files=()
	mapfile -d '' -t files < <(find "${dir}" -type f -print0 | LC_ALL=C sort -z || true)
	for file in "${files[@]}"; do
		audit_file "${file}" "${prefix}${file#"${dir}"/}"
		n=$((n + 1))
	done
	AUDIT_TREE_FILES=${n}
}

audit_input() {
	local input=${1%/} dir sum
	if [[ ! -e "${input}" ]]; then
		printf 'input %s files 0\n' "${input}"
		fail_line "${input} input does not exist"
		return 0
	fi
	if [[ -d "${input}" ]]; then
		audit_tree "${input}" "${input}/"
	elif [[ "${input}" == *.zip ]]; then
		sum=$(sha256sum <"${input}")
		printf 'sha256 %s\n' "${sum%% *}"
		need_tool unzip || return 0
		new_temp_dir dir || return 0
		if ! unzip -q -- "${input}" -d "${dir}" >/dev/null 2>&1; then
			fail_line "${input} could not be extracted"
			return 0
		fi
		audit_tree "${dir}" "${input}!"
	else
		audit_file "${input}" "${input}"
		AUDIT_TREE_FILES=1
	fi
	printf 'input %s files %s\n' "${input}" "${AUDIT_TREE_FILES}"
	[[ "${AUDIT_TREE_FILES}" -gt 0 ]] || fail_line "${input} files 0"
}

# Reads CI_CHECK_IDENTITY (tracing off): entries split on commas, trimmed,
# empty ones dropped. Nothing of it is printed but the entry count.
identity_load() {
	local raw entry
	set +x
	ID_ENTRIES=()
	raw=${CI_CHECK_IDENTITY:-}
	while [[ -n "${raw}" ]]; do
		entry=${raw%%,*}
		if [[ "${raw}" == *,* ]]; then raw=${raw#*,}; else raw=''; fi
		entry=${entry#"${entry%%[![:space:]]*}"}
		entry=${entry%"${entry##*[![:space:]]}"}
		if [[ -n "${entry}" ]]; then ID_ENTRIES+=("${entry}"); fi
	done
}

identity_presence() {
	if [[ "${#ID_ENTRIES[@]}" -gt 0 ]]; then
		printf 'identity: present, %s entries\n' "${#ID_ENTRIES[@]}"
	else
		printf 'identity: absent\n'
	fi
}

# identity_main: text records on stdin, one per line, "<location><TAB><text>"
# (the location ends at the first tab). Prints the presence line, one
# "<location><TAB><k>" line per record whose text holds entry k (1-based), then
# the counts. Only the text is matched, and only k is ever printed.
identity_main() {
	local dir k records bad status hits
	local -a statuses=()
	if [[ $# -gt 0 ]]; then
		usage
		return 2
	fi
	if ! command -v -- grep >/dev/null 2>&1; then
		printf 'identity: FAIL missing tool grep\n'
		return 2
	fi
	identity_load
	new_temp_dir dir || return 2
	if ! cat >"${dir}/records"; then
		printf 'identity: FAIL the records on stdin could not be read\n'
		return 2
	fi
	bad=$(grep -c -v -F -e $'\t' -- "${dir}/records")
	if [[ "${bad}" -gt 0 ]]; then
		printf 'identity: FAIL %s records have no tab after their location\n' "${bad}"
		return 2
	fi
	records=$(awk 'END { print NR }' "${dir}/records")
	cut -f 1 -- "${dir}/records" >"${dir}/locations"
	cut -f 2- -- "${dir}/records" >"${dir}/text"
	identity_presence
	: >"${dir}/hits"
	for k in "${!ID_ENTRIES[@]}"; do
		grep -n -F -i -f /dev/fd/3 -- "${dir}/text" 3< <(printf '%s\n' "${ID_ENTRIES[k]}") |
			awk -F: -v k="$((k + 1))" '{ print $1 "\t" k }' >>"${dir}/hits"
		statuses=("${PIPESTATUS[@]}")
		if [[ "${statuses[0]}" -gt 1 ]]; then
			printf 'identity: FAIL grep exited %s\n' "${statuses[0]}"
			return 2
		fi
	done
	sort -t $'\t' -k 1,1n -k 2,2n -- "${dir}/hits" |
		awk -F'\t' 'NR == FNR { location[NR] = $0; next } { print location[$1] "\t" $2 }' "${dir}/locations" -
	hits=$(awk 'END { print NR }' "${dir}/hits")
	printf 'identity: records %s hits %s\n' "${records}" "${hits}"
	rm -rf -- "${dir}"
	status=0
	[[ "${hits}" -eq 0 ]] || status=1
	return "${status}"
}

audit_main() {
	local public=no input tool status=0 identity
	local -a inputs=()
	EXPECTED_VERSION=''
	while [[ $# -gt 0 ]]; do
		case "$1" in
		--public-runner) public=yes ;;
		--expected-version)
			EXPECTED_VERSION=${2:-}
			shift
			;;
		*) inputs+=("$1") ;;
		esac
		shift
	done
	FAILURES=()
	MISSING_TOOLS=''
	FILES=0
	PE_FILES=0
	HITS=0
	PE_MISSING=0
	load_shapes
	identity_load
	identity_presence
	if [[ "${#ID_ENTRIES[@]}" -eq 0 && "${public}" != yes ]]; then
		fail_line 'identity absent: CI_CHECK_IDENTITY is unset and --public-runner was not given'
	fi
	for tool in "${AUDIT_TOOLS[@]}"; do need_tool "${tool}" || status=1; done
	if [[ "${status}" -eq 0 ]]; then
		[[ "${#inputs[@]}" -gt 0 ]] || fail_line 'no input given'
		for input in "${inputs[@]}"; do audit_input "${input}"; done
		[[ "${PE_FILES}" -gt 0 ]] || fail_line 'all inputs pe 0'
	fi
	if [[ "${#FAILURES[@]}" -gt 0 ]]; then printf '%s\n' "${FAILURES[@]}"; fi
	identity=absent
	if [[ "${#ID_ENTRIES[@]}" -gt 0 ]]; then identity=present; fi
	printf 'audit: files %s pe %s hits %s pe-missing %s identity %s\n' "${FILES}" "${PE_FILES}" "${HITS}" \
		"${PE_MISSING}" "${identity}"
	[[ "${#FAILURES[@]}" -eq 0 ]]
}

# ---------------------------------------------------------------------------
# Self-test. Every fixture is spelled out here by hand, never built from the
# shape list, so a shape and its fixture cannot change together. Key fixtures
# are split inside their fixed prefix, so this file matches no key shape.

# Planted fixtures: shape label, then the text it must catch.
st_planted() {
	printf '%s\t%s\n' \
		path-home '/home/quill/.cargo/registry/src/lib.rs' \
		path-root '/root/.cargo/git/checkouts/lib.rs' \
		path-users '/Users/quill/Library/Caches/app.log' \
		path-mnt-drive '/mnt/d/work/app/src/main.rs' \
		path-tmp '/tmp/app-build/out.log' \
		path-drive-users 'C:\Users\quill\AppData\app.log' \
		path-drive-users 'C:\\Users\\quill\\AppData\\app.log' \
		path-drive-users 'C:/Users/quill/AppData/app.log' \
		path-drive-a 'D:\a\app\app\src\main.rs' \
		path-wsl-unc '\\wsl.localhost\distro\srv\app.log' \
		path-wsl-unc '//wsl$/distro/srv/app.log' \
		key-aiza 'AI''zaSyD0e1f2g3h4i5j6k7l8m9n0p1q2r3s4t5u' \
		key-aiza 'buildcfgAI''zaSyD0e1f2g3h4i5j6k7l8m9n0p1q2r3s4t5uNEXT' \
		key-sk-typed 's''k-proj-Qx7Lm2Np9Rs4Tv6Wz8Yb3Cd' \
		key-sk-typed 's''k-ant-api03-Gh5Jk7Lm9Np2Qr4St6' \
		key-sk-legacy 's''k-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2' \
		key-gh-token 'gh''p_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8' \
		key-gh-pat 'github''_pat_11ABCDEFG0123456789_abcdefghij' \
		key-akia 'AK''IAQ2W3E4R5T6Y7U8I9' \
		key-pem-private '-----BE''GIN RSA PRIVATE KEY-----'
}

# Traps: real strings of a release build that no shape may flag.
st_traps() {
	# shellcheck disable=SC2088 # literal strings of a build, not paths to expand
	printf '%s\n' \
		'~/.cargo/registry/src/index.crates.io-0000000000000000/serde-1.0.0/src/lib.rs' \
		'~/.cargo/git/checkouts/sample-0000000000000000/abc1234/src/lib.rs' \
		'/rustc/0123456789abcdef0123456789abcdef01234567/library/core/src/panicking.rs' \
		'disk-AbCdEfGhIjKlMnOpQrStUvWx' \
		'https://aistudio.google.com/apikey' \
		'src\ai\gemini.rs' \
		'dontAsk--model--system-prompt'
}

# The three llvm-readobj views of a complete PE, shaped like LLVM 18's output
# with invented values.
st_view_headers() {
	cat <<'VIEW'
File: fixture.exe
Format: COFF-x86-64
Arch: x86_64
AddressSize: 64bit
ImageFileHeader {
  Machine: IMAGE_FILE_MACHINE_AMD64 (0x8664)
  SectionCount: 6
  Characteristics [ (0x22)
    IMAGE_FILE_EXECUTABLE_IMAGE (0x2)
    IMAGE_FILE_LARGE_ADDRESS_AWARE (0x20)
  ]
}
ImageOptionalHeader {
  Magic: 0x20B
  Subsystem: IMAGE_SUBSYSTEM_WINDOWS_GUI (0x2)
  Characteristics [ (0xC160)
    IMAGE_DLL_CHARACTERISTICS_DYNAMIC_BASE (0x40)
    IMAGE_DLL_CHARACTERISTICS_GUARD_CF (0x4000)
    IMAGE_DLL_CHARACTERISTICS_HIGH_ENTROPY_VA (0x20)
    IMAGE_DLL_CHARACTERISTICS_NX_COMPAT (0x100)
    IMAGE_DLL_CHARACTERISTICS_TERMINAL_SERVER_AWARE (0x8000)
  ]
}
VIEW
}

st_view_load() {
	cat <<'VIEW'
LoadConfig [
  Size: 0x140
  SecurityCookie: 0x1400A2F40
  GuardCFFunctionTable: 0x1400B1000
  GuardCFFunctionCount: 2048
  GuardFlags [ (0x10500)
    CF_FUNCTION_TABLE_PRESENT (0x400)
    CF_INSTRUMENTED (0x100)
    CF_LONGJUMP_TABLE_PRESENT (0x10000)
  ]
]
VIEW
}

st_view_debug() {
	cat <<'VIEW'
DebugDirectory [
  DebugEntry {
    Type: CodeView (0x2)
    PDBInfo {
      PDBAge: 1
      PDBFileName: fixture.pdb
    }
  }
  DebugEntry {
    Type: ExtendedDLLCharacteristics (0x14)
    ExtendedCharacteristics [ (0x1)
      IMAGE_DLL_CHARACTERISTICS_EX_CET_COMPAT (0x1)
    ]
  }
]
VIEW
}

# st_pe_case <view to change: headers|load|debug> <sed expression> <expected missing property>
st_pe_case() {
	local headers load debug out present
	headers=$(st_view_headers)
	load=$(st_view_load)
	debug=$(st_view_debug)
	case "$1" in
	headers) headers=$(sed -e "$2" <<<"${headers}") ;;
	load) load=$(sed -e "$2" <<<"${load}") ;;
	debug) debug=$(sed -e "$2" <<<"${debug}") ;;
	*) ;;
	esac
	PE_MISSING_NAMES=()
	out=$(pe_check "${headers}" "${load}" "${debug}")
	PE_MISSING_NAMES=()
	pe_check "${headers}" "${load}" "${debug}" >/dev/null
	ST_PE_CASES=$((ST_PE_CASES + 1))
	present=$(grep -c ': present$' <<<"${out}")
	if [[ -z "$3" && "${PE_MISSING_NOW}" -eq 0 && "${present}" -eq 10 ]]; then return 0; fi
	if [[ -n "$3" && "${PE_MISSING_NOW}" -eq 1 && "${PE_MISSING_NAMES[0]:-}" == "$3" ]]; then return 0; fi
	ST_PROBLEMS+=("the PE view change '$2' did not report exactly '${3:-nothing}' missing")
}

st_pe() {
	st_pe_case headers '' ''
	st_pe_case headers '/Machine: IMAGE_FILE_MACHINE_AMD64/d' machine
	st_pe_case headers '/IMAGE_FILE_LARGE_ADDRESS_AWARE/d' large-address-aware
	st_pe_case headers '/IMAGE_DLL_CHARACTERISTICS_DYNAMIC_BASE/d' aslr
	st_pe_case headers '/IMAGE_DLL_CHARACTERISTICS_HIGH_ENTROPY_VA/d' aslr
	st_pe_case headers '/IMAGE_DLL_CHARACTERISTICS_NX_COMPAT/d' dep
	st_pe_case headers '/Subsystem: IMAGE_SUBSYSTEM_WINDOWS_GUI/d' gui-subsystem
	st_pe_case load '/SecurityCookie:/d' stack-cookie
	st_pe_case load 's/SecurityCookie: .*/SecurityCookie: 0x0/' stack-cookie
	st_pe_case debug '/PDBFileName:/d' pdb-name-without-path
	st_pe_case debug 's#PDBFileName: .*#PDBFileName: build/launcher.pdb#' pdb-name-without-path
	st_pe_case headers '/IMAGE_DLL_CHARACTERISTICS_GUARD_CF/d' control-flow-guard
	st_pe_case load '/CF_INSTRUMENTED/d' control-flow-guard-table
	st_pe_case load 's/GuardCFFunctionCount: .*/GuardCFFunctionCount: 0/' control-flow-guard-table
	st_pe_case load '/GuardCFFunctionCount:/d' control-flow-guard-table
	st_pe_case debug '/IMAGE_DLL_CHARACTERISTICS_EX_CET_COMPAT/d' cet-compatible
}

# Writes one fixture as <dir>/<name>.a (ASCII) and <dir>/<name>.u (UTF-16LE).
st_write() {
	printf '%s\n' "$3" >"$1/$2.a"
	printf '%s\n' "$3" | iconv -f ASCII -t UTF-16LE >"$1/$2.u"
}

# Reads an audit's output into ST_HIT["<file> <shape>"] counts.
st_parse() {
	local line file=''
	ST_HIT=()
	while IFS= read -r line; do
		if [[ "${line}" =~ ^file\ ([^ ]+)\ bytes ]]; then
			file=${BASH_REMATCH[1]}
		elif [[ -n "${file}" && "${line}" =~ ^\ \ ([a-z0-9-]+)\ -\>\ ([0-9]+)$ ]]; then
			ST_HIT["${file} ${BASH_REMATCH[1]}"]=${BASH_REMATCH[2]}
		fi
	done <<<"$1"
}

st_planted_and_traps() {
	local dir=$1 label text n=0 enc out i
	local -A covered=()
	mkdir -p "${dir}/planted" "${dir}/traps"
	while IFS=$'\t' read -r label text; do
		n=$((n + 1))
		st_write "${dir}/planted" "$(printf '%02d' "${n}")-${label}" "${text}"
		ST_PLANTED_TEXT+=("${text}")
		ST_PLANTED_LABELS+=("${label}")
	done < <(st_planted || true)
	n=0
	while IFS= read -r text; do
		n=$((n + 1))
		st_write "${dir}/traps" "trap-${n}" "${text}"
	done < <(st_traps || true)
	out=$(audit_main --public-runner "${dir}/planted" 2>&1)
	ST_OUTPUTS+=("${out}")
	st_parse "${out}"
	for file in "${dir}"/planted/*; do
		label=${file##*/}
		enc=${label##*.}
		label=${label%.*}
		label=${label#*-}
		ST_PLANTED=$((ST_PLANTED + 1))
		if [[ "${ST_HIT["${file} ${label}"]:-0}" -ge 1 ]]; then
			ST_CAUGHT=$((ST_CAUGHT + 1))
			covered["${label} ${enc}"]=1
		else
			ST_PROBLEMS+=("shape ${label} missed the planted fixture ${file##*/}")
		fi
	done
	for label in "${ST_PLANTED_LABELS[@]}"; do
		[[ " ${SHAPE_LABEL[*]} " == *" ${label} "* ]] || ST_PROBLEMS+=("the planted label ${label} has no shape")
	done
	for i in "${!SHAPE_LABEL[@]}"; do
		for enc in a u; do
			[[ -n "${covered["${SHAPE_LABEL[i]} ${enc}"]+set}" ]] ||
				ST_PROBLEMS+=("shape ${SHAPE_LABEL[i]} has no caught fixture in $([[ "${enc}" == a ]] && echo ASCII || echo UTF-16LE)")
		done
	done
	out=$(audit_main --public-runner "${dir}/traps" 2>&1)
	ST_OUTPUTS+=("${out}")
	st_parse "${out}"
	for file in "${dir}"/traps/*; do
		ST_TRAPS=$((ST_TRAPS + 1))
		for i in "${!SHAPE_LABEL[@]}"; do
			if [[ "${ST_HIT["${file} ${SHAPE_LABEL[i]}"]:-0}" -gt 0 ]]; then
				ST_FLAGGED=$((ST_FLAGGED + 1))
				ST_PROBLEMS+=("shape ${SHAPE_LABEL[i]} flags the trap ${file##*/}")
			fi
		done
	done
}

st_identity() {
	local dir=$1 switch out
	mkdir -p "${dir}/identity"
	st_write "${dir}/identity" id 'built on a host of Quill-Feather-Ident today'
	for switch in '' --public-runner; do
		# The entry is invented; the caller's own CI_CHECK_IDENTITY is never read.
		out=$(CI_CHECK_IDENTITY=' , quill-feather-IDENT ,, ' audit_main ${switch:+"${switch}"} "${dir}/identity" 2>&1)
		ST_OUTPUTS+=("${out}")
		if ! grep -q -x -F "audit: FAIL ${dir}/identity/id.a identity 1" <<<"${out}" ||
			! grep -q -x -F "audit: FAIL ${dir}/identity/id.u identity 1" <<<"${out}"; then
			ST_PROBLEMS+=("the identity entry was not caught in both encodings ${switch:-without the switch}")
		fi
		if grep -q -i -F 'quill-feather-ident' <<<"${out}"; then
			ST_PROBLEMS+=('the identity entry appears in the output')
		fi
	done
}

# st_identity_record_case <label> <entries> <records file> <expected exit> <expected hit lines, newline-separated>
st_identity_record_case() {
	local out status=0 want got
	out=$(CI_CHECK_IDENTITY=$2 main identity <"$3" 2>&1) || status=$?
	printf '%s\n' "${out}" >>"${ST_IDM_DIR}/outputs"
	want=$(grep -c -v -e '^$' <<<"$5")
	got=$(grep -v -e '^identity: ' <<<"${out}")
	ST_IDM_PLANTED=$((ST_IDM_PLANTED + want))
	if [[ "${status}" -ne "$4" ]]; then
		ST_IDM_PROBLEMS+=("identity mode case $1 exited ${status}, not $4")
	elif [[ "${got}" != "$5" ]]; then
		ST_IDM_PROBLEMS+=("identity mode case $1 did not print exactly its expected hit lines")
	else
		ST_IDM_CAUGHT=$((ST_IDM_CAUGHT + want))
	fi
}

# The identity mode over literal records. Runs in a subshell: every entry is
# invented and set here, so the caller's own CI_CHECK_IDENTITY is never read.
st_identity_mode() {
	local dir=$1 tab=$'\t'
	mkdir -p "${dir}/idm"
	(
		unset CI_CHECK_IDENTITY
		ST_IDM_DIR="${dir}/idm"
		ST_IDM_PLANTED=0 ST_IDM_CAUGHT=0
		ST_IDM_PROBLEMS=()
		printf 'loc-1\tfirst line\nloc-2\tthe second line\nloc-3\tthird\n' >"${dir}/idm/plain"
		printf 'loc-1\tfirst line\nloc-2\tbuilt by Quill-Feather-IDENT here\nloc-3\tthird\n' >"${dir}/idm/one"
		printf 'loc-1\tno entry here\nquill-feather-ident\tclean text\nloc-3\ta\tb quill-FEATHER-ident\n' >"${dir}/idm/tabs"
		out=$(main identity <"${dir}/idm/plain" 2>&1) || ST_IDM_PROBLEMS+=('identity mode without entries did not exit 0')
		grep -q -x -F 'identity: absent' <<<"${out}" || ST_IDM_PROBLEMS+=('identity mode without entries did not print absent')
		grep -q -x -F 'identity: records 3 hits 0' <<<"${out}" || ST_IDM_PROBLEMS+=('identity mode without entries did not count 3 records and 0 hits')
		st_identity_record_case 'one entry, mixed case' 'quill-feather-ident' "${dir}/idm/one" 1 "loc-2${tab}1"
		st_identity_record_case 'the second of two entries' 'first-sample-entry,quill-feather-ident' "${dir}/idm/one" 1 "loc-2${tab}2"
		st_identity_record_case 'text after a second tab' 'quill-feather-ident' "${dir}/idm/tabs" 1 "loc-3${tab}1"
		out=$(CI_CHECK_IDENTITY=' one , ,two' main identity <"${dir}/idm/plain" 2>&1)
		grep -q -x -F 'identity: present, 2 entries' <<<"${out}" || ST_IDM_PROBLEMS+=('identity mode did not count 2 entries in a padded list')
		if grep -q -i -w -e one -e two <<<"${out}"; then ST_IDM_PROBLEMS+=('the identity mode printed an entry'); fi
		if grep -q -i -F -e 'feather-ident' -e 'first-sample' "${dir}/idm/outputs"; then
			ST_IDM_PROBLEMS+=('the identity mode printed an entry')
		fi
		printf '%s %s\n' "${ST_IDM_PLANTED}" "${ST_IDM_CAUGHT}"
		if [[ "${#ST_IDM_PROBLEMS[@]}" -gt 0 ]]; then printf '%s\n' "${ST_IDM_PROBLEMS[@]}"; fi
	) >"${dir}/idm.out"
}

# st_refusal <expected failure line> <audit arguments...>: an audit that must exit non-zero.
st_refusal() {
	local want=$1 out status=0
	shift
	out=$(audit_main "$@" 2>&1) || status=$?
	ST_REFUSALS=$((ST_REFUSALS + 1))
	if [[ "${status}" -ne 0 ]] && grep -q -x -F "${want}" <<<"${out}"; then
		ST_REFUSED=$((ST_REFUSED + 1))
	else
		ST_PROBLEMS+=("the refusal '${want}' did not happen (exit ${status})")
	fi
}

st_refusals() {
	local dir=$1 tool path
	mkdir -p "${dir}/empty" "${dir}/nope" "${dir}/badpe" "${dir}/bin"
	printf 'plain text\n' >"${dir}/nope/readme.txt"
	printf 'MZ this is no PE file at all\n' >"${dir}/badpe/fake.exe"
	for tool in "${AUDIT_TOOLS[@]}"; do
		path=$(command -v -- "${tool}") && ln -s "${path}" "${dir}/bin/${tool}"
	done
	(
		unset CI_CHECK_IDENTITY
		ST_PROBLEMS=()
		st_refusal "audit: FAIL ${dir}/no-such-input input does not exist" --public-runner "${dir}/no-such-input"
		st_refusal "audit: FAIL ${dir}/empty files 0" --public-runner "${dir}/empty"
		st_refusal 'audit: FAIL all inputs pe 0' --public-runner "${dir}/nope"
		st_refusal "audit: FAIL ${dir}/badpe/fake.exe llvm-readobj could not read it" --public-runner "${dir}/badpe"
		PATH="${dir}/bin" st_refusal 'audit: FAIL missing tool llvm-readobj' --public-runner "${dir}/badpe"
		st_refusal 'audit: FAIL identity absent: CI_CHECK_IDENTITY is unset and --public-runner was not given' "${dir}/nope"
		printf '%s %s %s\n' "${ST_REFUSALS}" "${ST_REFUSED}" "${#ST_PROBLEMS[@]}"
		if [[ "${#ST_PROBLEMS[@]}" -gt 0 ]]; then printf '%s\n' "${ST_PROBLEMS[@]}"; fi
	) >"${dir}/refusals.out"
}

self_test() {
	local dir text output line tool planted caught
	local -a result=()
	for tool in strings grep sha256sum iconv llvm-readobj; do
		command -v -- "${tool}" >/dev/null 2>&1 || {
			printf 'self-test: FAIL missing tool %s\n' "${tool}"
			return 1
		}
	done
	dir=$(mktemp -d) || return 1
	TEMP_DIRS+=("${dir}")
	mkdir -p "${dir}/tmp"
	export TMPDIR="${dir}/tmp"
	ST_PLANTED=0 ST_CAUGHT=0 ST_TRAPS=0 ST_FLAGGED=0 ST_REFUSALS=0 ST_REFUSED=0 ST_PE_CASES=0
	ST_PROBLEMS=()
	ST_OUTPUTS=()
	ST_PLANTED_TEXT=()
	ST_PLANTED_LABELS=()
	declare -gA ST_HIT=()
	load_shapes
	st_planted_and_traps "${dir}"
	st_identity "${dir}"
	st_pe
	st_refusals "${dir}"
	mapfile -t result <"${dir}/refusals.out"
	read -r ST_REFUSALS ST_REFUSED _ <<<"${result[0]}"
	ST_PROBLEMS+=("${result[@]:1}")
	st_identity_mode "${dir}"
	mapfile -t result <"${dir}/idm.out"
	read -r planted caught <<<"${result[0]}"
	ST_PLANTED=$((ST_PLANTED + planted))
	ST_CAUGHT=$((ST_CAUGHT + caught))
	ST_PROBLEMS+=("${result[@]:1}")
	# No planted text may reach the output of any audit over the fixtures.
	for output in "${ST_OUTPUTS[@]}"; do
		for text in "${ST_PLANTED_TEXT[@]}"; do
			if grep -q -F -- "${text}" <<<"${output}"; then ST_PROBLEMS+=("an audit printed a planted string"); fi
		done
	done
	for line in "${ST_PROBLEMS[@]}"; do printf 'self-test: FAIL %s\n' "${line}"; done
	printf 'self-test: planted %s caught %s traps %s flagged %s refusals %s/%s\n' "${ST_PLANTED}" "${ST_CAUGHT}" \
		"${ST_TRAPS}" "${ST_FLAGGED}" "${ST_REFUSED}" "${ST_REFUSALS}"
	[[ "${#ST_PROBLEMS[@]}" -eq 0 && "${ST_CAUGHT}" -eq "${ST_PLANTED}" && "${ST_PLANTED}" -ge 30 &&
		"${ST_TRAPS}" -ge 14 && "${ST_FLAGGED}" -eq 0 && "${ST_REFUSED}" -eq "${ST_REFUSALS}" && "${ST_PE_CASES}" -gt 0 ]]
}

usage() {
	printf 'usage: binary-audit.sh audit [--public-runner] [--expected-version <v>] <input>... | self-test | shapes | identity\n' >&2
}

main() {
	case "${1:-}" in
	audit)
		shift
		audit_main "$@"
		;;
	self-test) self_test ;;
	shapes) shapes_table ;;
	identity)
		shift
		identity_main "$@"
		;;
	*)
		usage
		return 2
		;;
	esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
	main "$@"
	exit "$?"
fi
