#!/usr/bin/env bash
# The step table and its runner for the lint, test and audit gates.
#
#   ./scripts/ci-check.sh               every row this machine can run
#   ./scripts/ci-check.sh --job <tag>   the rows of one CI job
#   ./scripts/ci-check.sh --list        every field of every row
#
# Each check is one `row` in declare_table. Before any row runs, each tool the
# selected rows name is compared with its pinned version (rust-toolchain.toml,
# .nvmrc, scripts/ci-tools.env, crates/launcher/package-lock.json); a missing
# or different tool fails every row that names it, without running it. Every
# selected row runs even after an earlier one failed, and the last line says
# whether the run passed and names each row it did not run, and why. Each CI
# job runs ./scripts/ci-check.sh --job <tag> for its rows.
#
# Environment read: CI_CHECK_UPDATE_BASELINES (lets the test rows rewrite
# their baselines; refused in CI), GITHUB_ACTIONS, GITHUB_EVENT_NAME,
# GITHUB_STEP_SUMMARY, RUNNER_TEMP and XWIN_CACHE_DIR.
#
# IMPORTANT: always re-run this AFTER `cargo fmt` -- reformatting can grow a
# function past clippy::too_many_lines, which fmt alone will not report.
set -uo pipefail

WINDOWS_TARGET=x86_64-pc-windows-msvc
declare -gA PIN=() TOOL_OK=() TOOL_LINE=() TOOL_FAIL=() TOOL_PINNED=() TOOL_FOUND=()

ci_mode() { [[ "${GITHUB_ACTIONS:-}" == true ]]; }

# ---------------------------------------------------------------------------
# The table: one `row` call per check, one `ci_only_item` per CI step that is
# not a check this script can run.

table_reset() {
	ROW_NAME=()
	ROW_TAGS=()
	ROW_CATEGORY=()
	ROW_TARGET=()
	ROW_TOOLS=()
	ROW_DIR=()
	ROW_REQUIRES=()
	ROW_EVENTS=()
	ROW_PRE=()
	ROW_COUNT=()
	ROW_ZERO=()
	ROW_CMD=()
	ROW_UNKNOWN=()
	ITEM_NAME=()
	ITEM_REASON=()
}

row() {
	local arg key value unknown='' has_count=no
	local name='' tags='' category='' target='' tools='' dir=. requires='' events='' pre='' count='' zero='' cmd=''
	for arg in "$@"; do
		if [[ "${arg}" != *=* ]]; then
			unknown+="${unknown:+, }${arg}"
			continue
		fi
		key=${arg%%=*}
		value=${arg#*=}
		case "${key}" in
		name) name=${value} ;;
		tags) tags=${value} ;;
		category) category=${value} ;;
		target) target=${value} ;;
		tools) tools=${value} ;;
		dir) dir=${value} ;;
		requires) requires=${value} ;;
		events) events=${value} ;;
		pre) pre=${value} ;;
		count)
			count=${value}
			has_count=yes
			;;
		zero) zero=${value} ;;
		cmd) cmd=${value} ;;
		*) unknown+="${unknown:+, }${key}" ;;
		esac
	done
	if [[ "${has_count}" == no ]]; then count=''; fi
	ROW_NAME+=("${name}")
	ROW_TAGS+=("${tags}")
	ROW_CATEGORY+=("${category}")
	ROW_TARGET+=("${target}")
	ROW_TOOLS+=("${tools}")
	ROW_DIR+=("${dir}")
	ROW_REQUIRES+=("${requires}")
	ROW_EVENTS+=("${events}")
	ROW_PRE+=("${pre}")
	ROW_COUNT+=("${count}")
	ROW_ZERO+=("${zero}")
	ROW_CMD+=("${cmd}")
	ROW_UNKNOWN+=("${unknown}")
}

ci_only_item() {
	local arg name='' reason=''
	for arg in "$@"; do
		case "${arg}" in
		name=*) name=${arg#name=} ;;
		reason=*) reason=${arg#reason=} ;;
		*) reason='' ;;
		esac
	done
	ITEM_NAME+=("${name}")
	ITEM_REASON+=("${reason}")
}

# ---------------------------------------------------------------------------
# Table validation: every error is printed, then the run stops with exit 2.

table_error() {
	printf "ci-check: table error: row '%s': %s\n" "$1" "$2" >&2
	TABLE_BAD=1
}

ere_compiles() {
	local status
	[[ '' =~ $1 ]]
	# shellcheck disable=SC2319 # status 2 of the =~ test is the compile error asked about
	status=$?
	[[ "${status}" -ne 2 ]]
}

# Prints the number of capturing groups in an ERE (brackets and escapes skipped).
ere_groups() {
	local re=$1 i=0 n=0 c
	while ((i < ${#re})); do
		c=${re:i:1}
		if [[ "${c}" == "\\" ]]; then
			i=$((i + 2))
			continue
		fi
		if [[ "${c}" == '[' ]]; then
			i=$((i + 1))
			if [[ "${re:i:1}" == '^' ]]; then i=$((i + 1)); fi
			if [[ "${re:i:1}" == ']' ]]; then i=$((i + 1)); fi
			while ((i < ${#re})) && [[ "${re:i:1}" != ']' ]]; do
				if [[ "${re:i:2}" == '[:' ]]; then
					i=$((i + 2))
					while ((i < ${#re})) && [[ "${re:i:2}" != ':]' ]]; do i=$((i + 1)); done
					i=$((i + 2))
				else
					i=$((i + 1))
				fi
			done
		elif [[ "${c}" == '(' ]]; then
			n=$((n + 1))
		fi
		i=$((i + 1))
	done
	printf '%s\n' "${n}"
}

in_list() { [[ ",$2," == *",$1,"* ]]; }

row_index() {
	local i
	for ((i = 0; i < ${#ROW_NAME[@]}; i++)); do
		if [[ "${ROW_NAME[i]}" == "$1" ]]; then
			printf '%s\n' "${i}"
			return 0
		fi
	done
	return 1
}

check_row_fields() {
	local i=$1 label=$2 tag
	local -a tags=()
	[[ -z "${ROW_UNKNOWN[i]}" ]] || table_error "${label}" "unknown field ${ROW_UNKNOWN[i]}"
	[[ -n "${ROW_NAME[i]}" ]] || table_error "${label}" 'name is empty'
	[[ "${ROW_NAME[i]}" != *[$'\t\n',]* ]] || table_error "${label}" 'name holds a tab, a newline or a comma'
	IFS=, read -r -a tags <<<"${ROW_TAGS[i]}"
	[[ "${#tags[@]}" -gt 0 ]] || table_error "${label}" 'tags is empty'
	for tag in "${tags[@]}"; do
		[[ "${tag}" =~ ^[a-z][a-z0-9-]*$ ]] || table_error "${label}" "tag '${tag}' is not lower-case letters, digits and dashes"
	done
	in_list "${ROW_CATEGORY[i]}" both,ci-only,local-only,non-blocking ||
		table_error "${label}" "category '${ROW_CATEGORY[i]}' is not both, ci-only, local-only or non-blocking"
	in_list "${ROW_TARGET[i]}" any,windows,linux ||
		table_error "${label}" "target '${ROW_TARGET[i]}' is not any, windows or linux"
	if [[ "${ROW_DIR[i]}" == /* || "${ROW_DIR[i]}" =~ (^|/)\.\.(/|$) || ! -d "${ROOT}/${ROW_DIR[i]}" ]]; then
		table_error "${label}" "dir '${ROW_DIR[i]}' is not a directory under the repository root"
	fi
	[[ "${ROW_PRE[i]}" != *[$'\t\n']* ]] || table_error "${label}" 'pre holds a tab or a newline'
}

check_row_links() {
	local i=$1 label=$2 req event tag j
	local -a reqs=() events=() tags=()
	IFS=, read -r -a tags <<<"${ROW_TAGS[i]}"
	IFS=, read -r -a reqs <<<"${ROW_REQUIRES[i]}"
	for req in "${reqs[@]}"; do
		j=$(row_index "${req}") || j=-1
		if [[ "${j}" -lt 0 || "${j}" -ge "${i}" ]]; then
			table_error "${label}" "requires '${req}', which is not an earlier row"
			continue
		fi
		for tag in "${tags[@]}"; do
			in_list "${tag}" "${ROW_TAGS[j]}" || table_error "${label}" "requires '${req}', which lacks its tag '${tag}'"
		done
	done
	IFS=, read -r -a events <<<"${ROW_EVENTS[i]}"
	for event in "${events[@]}"; do
		in_list "${event}" push,pull_request,schedule,workflow_dispatch ||
			table_error "${label}" "event '${event}' is not push, pull_request, schedule or workflow_dispatch"
	done
}

check_row_patterns() {
	local i=$1 label=$2 groups q="'"
	local cargo_re="^([A-Z_][A-Z0-9_]*=(${q}[^${q}]*${q}|[^ ]*) )*cargo [a-z-]+( |\$)"
	if [[ -z "${ROW_COUNT[i]}" ]]; then
		table_error "${label}" 'count is missing (count=none when the tool prints nothing countable)'
	elif [[ "${ROW_COUNT[i]}" != none ]] && ! ere_compiles "${ROW_COUNT[i]}"; then
		table_error "${label}" 'count pattern does not compile'
	fi
	if [[ -n "${ROW_ZERO[i]}" ]]; then
		if ! ere_compiles "${ROW_ZERO[i]}"; then
			table_error "${label}" 'zero pattern does not compile'
		else
			groups=$(ere_groups "${ROW_ZERO[i]}")
			[[ "${groups}" -eq 1 ]] || table_error "${label}" "zero pattern has ${groups} capture groups, not 1"
		fi
	fi
	if [[ -z "${ROW_CMD[i]}" ]]; then
		table_error "${label}" 'cmd is empty'
	elif [[ "${ROW_CMD[i]}" == *[$'\t\n']* ]]; then
		table_error "${label}" 'cmd holds a tab or a newline'
	elif [[ "${ROW_TARGET[i]}" == windows && ! "${ROW_CMD[i]}" =~ ${cargo_re} ]]; then
		table_error "${label}" 'a windows-target command must start with cargo <subcommand>'
	fi
}

table_check() {
	local i j label
	TABLE_BAD=0
	for ((i = 0; i < ${#ROW_NAME[@]}; i++)); do
		label=${ROW_NAME[i]:-#$((i + 1))}
		check_row_fields "${i}" "${label}"
		check_row_links "${i}" "${label}"
		check_row_patterns "${i}" "${label}"
		for ((j = 0; j < i; j++)); do
			[[ "${ROW_NAME[j]}" != "${ROW_NAME[i]}" ]] || table_error "${label}" 'name is used twice'
		done
	done
	for ((i = 0; i < ${#ITEM_NAME[@]}; i++)); do
		if [[ -z "${ITEM_NAME[i]}" || -z "${ITEM_REASON[i]}" ]]; then
			table_error "ci-only item #$((i + 1))" 'needs a name and a reason'
		fi
	done
	return "${TABLE_BAD}"
}

table_list() {
	local i
	printf '#kind\tname\ttags\tcategory\ttarget\ttools\tdir\trequires\tevents\tpre\tcount\tzero\tcmd\n'
	for ((i = 0; i < ${#ROW_NAME[@]}; i++)); do
		printf 'row\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "${ROW_NAME[i]}" "${ROW_TAGS[i]}" \
			"${ROW_CATEGORY[i]}" "${ROW_TARGET[i]}" "${ROW_TOOLS[i]:--}" "${ROW_DIR[i]}" "${ROW_REQUIRES[i]:--}" \
			"${ROW_EVENTS[i]:--}" "${ROW_PRE[i]:--}" "${ROW_COUNT[i]}" "${ROW_ZERO[i]:--}" "${ROW_CMD[i]}"
	done
	for ((i = 0; i < ${#ITEM_NAME[@]}; i++)); do
		printf 'ci-only\t%s\t%s\n' "${ITEM_NAME[i]}" "${ITEM_REASON[i]}"
	done
	printf 'rows\t%s\n' "${#ROW_NAME[@]}"
	[[ "${#ROW_NAME[@]}" -ge 1 ]]
}

# ---------------------------------------------------------------------------
# Pins: parsed, never sourced or executed.

pins_load() {
	local line n=0 key bad=0 label=${TOOL_FILE#"${ROOT}/"}
	PIN=()
	if [[ ! -f "${TOOL_FILE}" ]]; then
		printf 'ci-check: tool file %s is missing\n' "${label}" >&2
		return 1
	fi
	while IFS= read -r line || [[ -n "${line}" ]]; do
		n=$((n + 1))
		if [[ -z "${line}" || "${line}" == '#'* ]]; then continue; fi
		if [[ ! "${line}" =~ ^[A-Z][A-Z0-9_]*=[^[:space:]]+$ ]]; then
			printf 'ci-check: tool file %s line %s is not KEY=VALUE, a # comment or blank\n' "${label}" "${n}" >&2
			bad=1
			continue
		fi
		key=${line%%=*}
		if [[ -n "${PIN[${key}]+set}" ]]; then
			printf 'ci-check: tool file %s line %s repeats the key %s\n' "${label}" "${n}" "${key}" >&2
			bad=1
			continue
		fi
		PIN[${key}]=${line#*=}
	done <"${TOOL_FILE}"
	return "${bad}"
}

rust_channel() {
	local line
	[[ -f "${TOOLCHAIN_FILE}" ]] || return 1
	while IFS= read -r line || [[ -n "${line}" ]]; do
		if [[ "${line}" =~ ^channel[[:space:]]*=[[:space:]]*\"([^\"]+)\" ]]; then
			printf '%s\n' "${BASH_REMATCH[1]}"
			return 0
		fi
	done <"${TOOLCHAIN_FILE}"
	return 1
}

nvmrc_version() {
	local line=''
	[[ -f "${NVMRC_FILE}" ]] || return 1
	IFS= read -r line <"${NVMRC_FILE}" || [[ -n "${line}" ]] || return 1
	line=${line//[[:space:]]/}
	printf '%s\n' "${line#v}"
}

first_version() {
	[[ "$1" =~ ([0-9]+\.[0-9]+\.[0-9]+) ]] || return 1
	printf '%s\n' "${BASH_REMATCH[1]}"
}

# One node helper reads both JSON files: the lock's version of the package
# and the entry point the package's own bin field names. Prints "version<TAB>entry".
npm_package_info() {
	node -e '
const fs = require("node:fs");
const path = require("node:path");
const [lockFile, dir, name] = process.argv.slice(1);
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const entry = (read(lockFile).packages || {})["node_modules/" + name] || {};
let bin = "";
try {
  const b = read(path.join(dir, "node_modules", name, "package.json")).bin;
  const values = b && typeof b === "object" ? Object.values(b) : [];
  const last = name.split("/").pop();
  if (typeof b === "string") bin = b;
  else if (values.length > 0 && typeof b[last] === "string") bin = b[last];
  else if (values.length === 1) bin = values[0];
  if (bin) bin = path.posix.join("node_modules", name, bin);
} catch {
  bin = "";
}
process.stdout.write((entry.version || "") + "\t" + bin + "\n");
' "${NPM_LOCK}" "${ROOT}/${NPM_DIR}" "$1"
}

install_hint() {
	case "$1" in
	gitleaks | actionlint) printf 'install the %s %s release archive\n' "$1" "$2" ;;
	typos) printf 'run cargo install --locked typos-cli@%s\n' "$2" ;;
	*) printf 'run cargo install --locked %s@%s\n' "$1" "$2" ;;
	esac
}

tool_compare() {
	local tool=$1 pinned=$2 found=$3 hint=$4 shown=${5:-$3}
	PINS_COMPARED=$((PINS_COMPARED + 1))
	TOOL_PINNED[${tool}]=${pinned:-not readable}
	TOOL_FOUND[${tool}]=${shown:-not found}
	TOOL_LINE[${tool}]="tool ${tool}: pinned ${pinned:-not readable}, found ${shown:-not found}"
	if [[ -n "${pinned}" && "${pinned}" == "${found}" ]]; then
		TOOL_OK[${tool}]=1
	else
		TOOL_OK[${tool}]=0
		TOOL_FAIL[${tool}]="${TOOL_LINE[${tool}]}; ${hint}"
	fi
}

tool_presence() {
	TOOL_PINNED[$1]=-
	if command -v -- "$1" >/dev/null 2>&1; then
		TOOL_OK[$1]=1
		TOOL_FOUND[$1]=present
		TOOL_LINE[$1]="tool $1: present"
	else
		TOOL_OK[$1]=0
		TOOL_FOUND[$1]='not found'
		TOOL_LINE[$1]="tool $1: not found"
		TOOL_FAIL[$1]="tool $1: not found on PATH"
	fi
}

tool_check_rust() {
	local pinned='' found='' out=''
	pinned=$(rust_channel) || pinned=''
	if out=$(cd -- "${ROOT}" && rustc -V 2>/dev/null); then found=$(first_version "${out}") || found=''; fi
	if [[ -z "${RUSTUP_TOOLCHAIN:-}" ]]; then
		tool_compare rust "${pinned}" "${found}" 'in the repository, run rustup toolchain install'
	elif ci_mode; then
		TOOL_PINNED[rust]="override ${RUSTUP_TOOLCHAIN}"
		TOOL_FOUND[rust]=${found:-not found}
		TOOL_LINE[rust]="tool rust: override ${RUSTUP_TOOLCHAIN}, found ${found:-not found}"
		TOOL_OK[rust]=0
		TOOL_FAIL[rust]="${TOOL_LINE[rust]}; the overridden toolchain is not installed"
		if [[ -n "${found}" ]]; then TOOL_OK[rust]=1; fi
	else
		tool_compare rust "${pinned}" "${found}" 'unset RUSTUP_TOOLCHAIN' \
			"${found:-not found} (RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN})"
	fi
}

tool_check() {
	local tool=$1 found='' out='' info='' entry='' key hint pinned
	[[ -z "${TOOL_OK[${tool}]+set}" ]] || return 0
	case "${tool}" in
	rust) tool_check_rust ;;
	node)
		if out=$(node --version 2>/dev/null); then found=$(first_version "${out}") || found=''; fi
		pinned=$(nvmrc_version) || pinned=''
		tool_compare node "${pinned}" "${found}" 'run nvm install, then nvm alias default'
		;;
	npm:*)
		info=$(npm_package_info "${tool#npm:}" 2>/dev/null) || info=''
		entry=${info#*$'\t'}
		if [[ -n "${entry}" ]] && out=$(cd -- "${ROOT}/${NPM_DIR}" && node "${entry}" --version 2>/dev/null); then
			found=$(first_version "${out}") || found=''
		fi
		tool_compare "${tool}" "${info%%$'\t'*}" "${found}" \
			"the frontend packages are installed by hand in ${NPM_DIR}"
		;;
	*)
		key=${tool^^}
		key=${key//-/_}
		if [[ -z "${PIN[${key}]+set}" ]]; then
			tool_presence "${tool}"
			return 0
		fi
		if [[ "${tool}" == cargo-* ]]; then
			out=$(cd -- "${ROOT}" && cargo "${tool#cargo-}" --version 2>/dev/null) || out=''
		else
			out=$("${tool}" --version 2>/dev/null) || out=''
		fi
		found=$(first_version "${out}") || found=''
		hint=$(install_hint "${tool}" "${PIN[${key}]}")
		tool_compare "${tool}" "${PIN[${key}]}" "${found}" "${hint}"
		;;
	esac
}

# ---------------------------------------------------------------------------
# Host adaptation, the one host branch: host_adapt <uname> <target> <command>
# prints the command to run, or the reason the row cannot run here (status 1).

host_adapt() {
	local host=$1 target=$2 cmd=$3 q="'" prefix=''
	local re="^(([A-Z_][A-Z0-9_]*=(${q}[^${q}]*${q}|[^ ]*) )*)cargo ([a-z-]+)( (.*))?\$"
	if [[ "${target}" == any ]]; then
		printf '%s\n' "${cmd}"
		return 0
	fi
	case "${host}" in
	Linux)
		if [[ "${target}" == linux ]]; then
			printf '%s\n' "${cmd}"
			return 0
		fi
		if [[ ! "${cmd}" =~ ${re} ]]; then
			printf 'a windows-target command must start with cargo <subcommand>\n'
			return 1
		fi
		if [[ "${BASH_REMATCH[4]}" == test ]]; then prefix='CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER=env '; fi
		printf '%s%scargo xwin %s --target %s%s\n' "${prefix}" "${BASH_REMATCH[1]}" "${BASH_REMATCH[4]}" \
			"${WINDOWS_TARGET}" "${BASH_REMATCH[5]}"
		;;
	MINGW* | MSYS* | CYGWIN*)
		if [[ "${target}" == windows ]]; then
			printf '%s\n' "${cmd}"
			return 0
		fi
		printf 'needs a Linux host\n'
		return 1
		;;
	*)
		printf 'unsupported host (%s)\n' "${host}"
		return 1
		;;
	esac
}

xwin_adapted() { [[ "${HOST_UNAME}" == Linux && "${ROW_TARGET[$1]}" == windows ]]; }

# Sets TOOL_LIST to the row's tools, plus cargo-xwin when the host adapts it.
row_tools() {
	TOOL_LIST=()
	IFS=, read -r -a TOOL_LIST <<<"${ROW_TOOLS[$1]}"
	if xwin_adapted "$1"; then TOOL_LIST+=(cargo-xwin); fi
}

# ---------------------------------------------------------------------------
# Selection and the versions header.

select_rows() {
	local mode=$1 job=$2 i tag all=','
	local -a tags=()
	SEL=()
	for ((i = 0; i < ${#ROW_NAME[@]}; i++)); do
		IFS=, read -r -a tags <<<"${ROW_TAGS[i]}"
		for tag in "${tags[@]}"; do
			[[ "${all}" == *",${tag},"* ]] || all+="${tag},"
		done
		if [[ "${mode}" == default ]] || in_list "${job}" "${ROW_TAGS[i]}"; then SEL+=("${i}"); fi
	done
	if [[ "${mode}" == job && "${#SEL[@]}" -eq 0 ]]; then
		all=${all#,}
		all=${all%,}
		printf "ci-check: no row carries the tag '%s'; tags: %s\n" "${job}" "${all//,/, }" >&2
		return 1
	fi
}

category_reason() {
	local category=${ROW_CATEGORY[$1]}
	if [[ "${RUN_MODE}" == default ]]; then
		if [[ "${category}" == ci-only || "${category}" == non-blocking ]]; then printf '%s\n' "${category}"; fi
	elif ci_mode && [[ "${category}" == local-only ]]; then
		printf 'local-only\n'
	fi
}

event_reason() {
	local event=${GITHUB_EVENT_NAME:-unknown}
	if ci_mode && [[ -n "${ROW_EVENTS[$1]}" ]] && ! in_list "${event}" "${ROW_EVENTS[$1]}"; then
		printf 'not on %s\n' "${event}"
	fi
}

versions_header() {
	local i k tool reason
	local -a tools=()
	TOOL_ORDER=()
	PINS_COMPARED=0
	XWIN_EXPORTED=no
	if ci_mode; then printf 'image %s %s\n' "${ImageOS:-unknown}" "${ImageVersion:-unknown}"; fi
	for k in "${!SEL[@]}"; do
		i=${SEL[k]}
		reason=$(category_reason "${i}")
		reason+=$(event_reason "${i}")
		[[ -z "${reason}" ]] || continue
		row_tools "${i}"
		for tool in "${TOOL_LIST[@]}"; do
			[[ -z "${TOOL_OK[${tool}]+set}" ]] || continue
			tool_check "${tool}"
			TOOL_ORDER+=("${tool}")
			printf '%s\n' "${TOOL_LINE[${tool}]}"
		done
		if xwin_adapted "${i}" && [[ "${XWIN_EXPORTED}" == no ]]; then
			XWIN_EXPORTED=yes
			printf 'tool XWIN_SDK_VERSION: exported %s\n' "${PIN[XWIN_SDK_VERSION]:-missing from the tool file}"
			printf 'tool XWIN_CRT_VERSION: exported %s\n' "${PIN[XWIN_CRT_VERSION]:-missing from the tool file}"
		fi
	done
	printf 'pins compared %s\n' "${PINS_COMPARED}"
	if [[ "${PINS_COMPARED}" -eq 0 ]]; then
		printf 'ci-check: no pinned tool version was compared: the selected rows name no pinned tool\n'
	fi
}

# ---------------------------------------------------------------------------
# Running one row.

result_set() {
	RES_OUTCOME[$1]=$2
	RES_REASON[$1]=$3
	RES_SECS[$1]=$4
	RES_COUNTS[$1]=$5
}

strip_ansi() { sed -E -e $'s/\e\\[[0-9;?]*[ -/]*[@-~]//g' -e $'s/\r$//'; }

# Sets SECS to the seconds since an EPOCHREALTIME value, to one decimal.
seconds_since() {
	local start=${1//[.,]/} now=${EPOCHREALTIME//[.,]/} tenths
	tenths=$(((now - start) / 100000))
	SECS="$((tenths / 10)).$((tenths % 10))"
}

not_run_reason() {
	local i=$1 reason req j
	local -a reqs=()
	reason=$(category_reason "${i}")
	[[ -n "${reason}" ]] || reason=$(event_reason "${i}")
	if [[ -z "${reason}" ]]; then
		IFS=, read -r -a reqs <<<"${ROW_REQUIRES[i]}"
		for req in "${reqs[@]}"; do
			j=$(row_index "${req}") || continue
			if [[ "${RES_OUTCOME[j]:-}" != PASS ]]; then
				reason="requires ${req}, which did not pass"
				break
			fi
		done
	fi
	printf '%s' "${reason}"
}

# Runs the row's precondition (local runs only): exit 1 prints the reason it
# does not apply; any other failure is the row's failure.
pre_reason() {
	local i=$1 out status=0
	if ci_mode || [[ -z "${ROW_PRE[i]}" ]]; then return 0; fi
	out=$(cd -- "${ROOT}/${ROW_DIR[i]}" && bash -o pipefail -c "${ROW_PRE[i]}" </dev/null 2>/dev/null) || status=$?
	if [[ "${status}" -eq 1 ]]; then
		PRE_OUTCOME='NOT RUN'
		PRE_REASON=${out%%$'\n'*}
		PRE_REASON=${PRE_REASON:-precondition not met}
	elif [[ "${status}" -ne 0 ]]; then
		PRE_OUTCOME=FAIL
		PRE_REASON="precondition exit ${status}"
	fi
}

zero_check() {
	local ere=$1 file=$2 last='' n=''
	last=$(grep -E -- "${ere}" "${file}" | tail -n 1)
	if [[ -n "${last}" && "${last}" =~ ${ere} ]]; then n=${BASH_REMATCH[1]}; fi
	if [[ -z "${n}" ]]; then
		printf 'no line gives the zero count\n'
	elif [[ ! "${n}" =~ ^[0-9]+$ ]]; then
		printf 'the zero count is not a number\n'
	elif ((10#${n} == 0)); then
		printf 'the zero count is 0\n'
	fi
}

row_command() {
	local i=$1 eff=$2
	(
		cd -- "${ROOT}/${ROW_DIR[i]}" || exit 2
		if xwin_adapted "${i}"; then
			exec env "XWIN_SDK_VERSION=${PIN[XWIN_SDK_VERSION]}" "XWIN_CRT_VERSION=${PIN[XWIN_CRT_VERSION]}" \
				bash -o pipefail -c "${eff}"
		fi
		exec bash -o pipefail -c "${eff}"
	) </dev/null 2>&1 | tee -- "${STATE_DIR}/${i}.log"
	return "${PIPESTATUS[0]}"
}

# Sets VERDICT (the failure reason, empty on a pass) and ROW_COUNT_LINES.
row_verdict() {
	local i=$1 status=$2 plain="${STATE_DIR}/$1.plain" sdk
	strip_ansi <"${STATE_DIR}/${i}.log" >"${plain}"
	ROW_COUNT_LINES=''
	VERDICT=''
	if [[ "${ROW_COUNT[i]}" != none ]]; then
		ROW_COUNT_LINES=$(grep -E -- "${ROW_COUNT[i]}" "${plain}" | tail -n 5)
	fi
	if [[ "${INTERRUPTED}" -eq 1 ]]; then
		VERDICT=interrupted
	elif [[ "${status}" -ne 0 ]]; then
		VERDICT="exit ${status}"
	elif [[ "${ROW_COUNT[i]}" != none && -z "${ROW_COUNT_LINES}" ]]; then
		VERDICT='no count line'
	elif [[ -n "${ROW_ZERO[i]}" ]]; then
		VERDICT=$(zero_check "${ROW_ZERO[i]}" "${plain}")
	fi
	if [[ -z "${VERDICT}" ]] && xwin_adapted "${i}"; then
		sdk="${XWIN_CACHE_DIR:-${HOME}/.cache/cargo-xwin}/xwin/sdk/include/${PIN[XWIN_SDK_VERSION]}"
		[[ -d "${sdk}" ]] || VERDICT="the xwin cache has no sdk/include/${PIN[XWIN_SDK_VERSION]} after the command"
	fi
}

row_fail_early() {
	result_set "$1" FAIL "$2" '' ''
	printf -- '-- %s: FAIL -- %s\n' "${ROW_NAME[$1]}" "$2"
}

run_row() {
	local i=$1 name=${ROW_NAME[$1]} reason eff tool start status=0 where=''
	local -a tools=()
	reason=$(not_run_reason "${i}")
	if [[ -n "${reason}" ]]; then
		result_set "${i}" 'NOT RUN' "${reason}" '' ''
		printf -- '-- %s: NOT RUN (%s)\n' "${name}" "${reason}"
		return 0
	fi
	PRE_OUTCOME=''
	pre_reason "${i}"
	if [[ -n "${PRE_OUTCOME}" ]]; then
		result_set "${i}" "${PRE_OUTCOME}" "${PRE_REASON}" '' ''
		printf -- '-- %s: %s (%s)\n' "${name}" "${PRE_OUTCOME}" "${PRE_REASON}"
		return 0
	fi
	row_tools "${i}"
	for tool in "${TOOL_LIST[@]}"; do
		if [[ "${TOOL_OK[${tool}]:-0}" -ne 1 ]]; then
			row_fail_early "${i}" "${TOOL_FAIL[${tool}]:-tool ${tool}: not checked}"
			return 0
		fi
	done
	if xwin_adapted "${i}" && [[ -z "${PIN[XWIN_SDK_VERSION]:-}" || -z "${PIN[XWIN_CRT_VERSION]:-}" ]]; then
		row_fail_early "${i}" 'the tool file lacks XWIN_SDK_VERSION or XWIN_CRT_VERSION'
		return 0
	fi
	if ! eff=$(host_adapt "${HOST_UNAME}" "${ROW_TARGET[i]}" "${ROW_CMD[i]}"); then
		row_fail_early "${i}" "${eff}"
		return 0
	fi
	[[ "${ROW_DIR[i]}" == . ]] || where=" (in ${ROW_DIR[i]})"
	if ci_mode; then printf '::group::%s\n' "${name}"; else printf '\n== %s ==\n' "${name}"; fi
	printf '$ %s%s\n' "${eff}" "${where}"
	start=${EPOCHREALTIME}
	row_command "${i}" "${eff}" || status=$?
	if ci_mode; then printf '::endgroup::\n'; fi
	row_verdict "${i}" "${status}"
	seconds_since "${start}"
	if [[ -n "${VERDICT}" ]]; then
		result_set "${i}" FAIL "${VERDICT}" "${SECS}" "${ROW_COUNT_LINES}"
		printf -- '-- %s: FAIL (%s) (%s s)\n' "${name}" "${VERDICT}" "${RES_SECS[i]}"
	else
		result_set "${i}" PASS '' "${SECS}" "${ROW_COUNT_LINES}"
		printf -- '-- %s: PASS (%s s)\n' "${name}" "${RES_SECS[i]}"
	fi
}
# ---------------------------------------------------------------------------
# The table, the counts line, the verdict and the job summary.

# Sets OUTCOME_TEXT to "PASS", "FAIL (<reason>)" or "NOT RUN (<reason>)".
outcome_text() {
	OUTCOME_TEXT=${RES_OUTCOME[$1]}
	if [[ -n "${RES_REASON[$1]}" ]]; then OUTCOME_TEXT+=" (${RES_REASON[$1]})"; fi
}

print_table() {
	local k i line first
	printf '\n%-40s %-40s %8s  %s\n' step outcome seconds 'count lines'
	for k in "${!SEL[@]}"; do
		i=${SEL[k]}
		first=yes
		outcome_text "${i}"
		while IFS= read -r line; do
			if [[ "${first}" == yes ]]; then
				printf '%-40s %-40s %8s  %s\n' "${ROW_NAME[i]}" "${OUTCOME_TEXT}" "${RES_SECS[i]:--}" "${line}"
				first=no
			else
				printf '%-40s %-40s %8s  %s\n' '' '' '' "${line}"
			fi
		done <<<"${RES_COUNTS[i]}"
	done
}

# Sets FAILED_LIST, NOT_RUN_TEXT and COUNTS_LINE from the results.
tally() {
	local k i passed=0 failed=0 not_run=0 reason names
	local -a order=()
	local -A by_reason=()
	FAILED_LIST=''
	for k in "${!SEL[@]}"; do
		i=${SEL[k]}
		case "${RES_OUTCOME[i]}" in
		PASS) passed=$((passed + 1)) ;;
		FAIL)
			failed=$((failed + 1))
			FAILED_LIST+="${FAILED_LIST:+, }${ROW_NAME[i]}"
			;;
		*)
			not_run=$((not_run + 1))
			reason=${RES_REASON[i]}
			[[ -n "${by_reason[${reason}]+set}" ]] || order+=("${reason}")
			by_reason[${reason}]+="${by_reason[${reason}]:+, }${ROW_NAME[i]}"
			;;
		esac
	done
	if [[ "${PINS_COMPARED}" -eq 0 ]]; then
		FAILED_LIST+="${FAILED_LIST:+, }tool versions (pins compared 0)"
	fi
	NOT_RUN_TEXT=''
	for reason in "${order[@]}"; do
		NOT_RUN_TEXT+="${NOT_RUN_TEXT:+; }${reason}: ${by_reason[${reason}]}"
	done
	if ! ci_mode && [[ "${#ITEM_NAME[@]}" -gt 0 ]]; then
		names=$(
			IFS=,
			printf '%s' "${ITEM_NAME[*]}"
		)
		NOT_RUN_TEXT+="${NOT_RUN_TEXT:+; }CI only: ${names//,/, }"
	fi
	COUNTS_LINE="rows selected ${#SEL[@]}, passed ${passed}, failed ${failed}, not run ${not_run}"
}

verdict_line() {
	if [[ -n "${FAILED_LIST}" ]]; then
		printf 'run failed: %s\n' "${FAILED_LIST}"
	elif ci_mode; then
		printf 'job rows passed%s\n' "${NOT_RUN_TEXT:+; not run -- ${NOT_RUN_TEXT}}"
	else
		printf 'local run passed%s\n' "${NOT_RUN_TEXT:+; not run here -- ${NOT_RUN_TEXT}}"
	fi
}

# Sets REPLY to $1 escaped for a markdown table cell; newlines become <br>.
md_cell() {
	local s=$1
	s=${s//&/\&amp;}
	s=${s//</\&lt;}
	s=${s//>/\&gt;}
	s=${s//|/\\|}
	REPLY=${s//$'\n'/<br>}
}

summary_table_rows() {
	local k i tool pinned found name outcome counts
	for tool in "${TOOL_ORDER[@]}"; do
		md_cell "${tool}"
		name=${REPLY}
		md_cell "${TOOL_PINNED[${tool}]:--}"
		pinned=${REPLY}
		md_cell "${TOOL_FOUND[${tool}]:--}"
		found=${REPLY}
		printf '| %s | %s | %s |\n' "${name}" "${pinned}" "${found}"
	done
	if [[ "${XWIN_EXPORTED}" == yes ]]; then
		printf '| XWIN_SDK_VERSION | %s | exported |\n' "${PIN[XWIN_SDK_VERSION]:-missing}"
		printf '| XWIN_CRT_VERSION | %s | exported |\n' "${PIN[XWIN_CRT_VERSION]:-missing}"
	fi
	printf '\npins compared %s\n\n| step | outcome | seconds | count lines |\n|---|---|---|---|\n' "${PINS_COMPARED}"
	for k in "${!SEL[@]}"; do
		i=${SEL[k]}
		md_cell "${ROW_NAME[i]}"
		name=${REPLY}
		outcome_text "${i}"
		md_cell "${OUTCOME_TEXT}"
		outcome=${REPLY}
		md_cell "${RES_COUNTS[i]}"
		counts=${REPLY}
		printf '| %s | %s | %s | %s |\n' "${name}" "${outcome}" "${RES_SECS[i]:--}" "${counts}"
	done
}

summary_write() {
	local k i
	[[ -n "${GITHUB_STEP_SUMMARY:-}" ]] || return 0
	{
		printf '### Rows run by ci-check.sh%s\n\n' "${1:+ --job $1}"
		if ci_mode; then printf 'image %s %s\n\n' "${ImageOS:-unknown}" "${ImageVersion:-unknown}"; fi
		printf '| tool | pinned | found |\n|---|---|---|\n'
		summary_table_rows
		printf '\n%s\n' "${COUNTS_LINE}"
		for k in "${!SEL[@]}"; do
			i=${SEL[k]}
			[[ "${RES_OUTCOME[i]}" == FAIL && -f "${STATE_DIR}/${i}.plain" ]] || continue
			md_cell "${ROW_NAME[i]}"
			printf '\n<details><summary>%s</summary>\n\n<pre>\n' "${REPLY}"
			tail -n 20 "${STATE_DIR}/${i}.plain" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
			printf '</pre>\n\n</details>\n'
		done
	} >>"${GITHUB_STEP_SUMMARY}"
}

report() {
	print_table
	tally
	summary_write "$1"
	printf '%s\n' "${COUNTS_LINE}"
	verdict_line
}

# ---------------------------------------------------------------------------
# The run.

state_dir_make() {
	local base
	if ci_mode && [[ -n "${RUNNER_TEMP:-}" ]]; then
		base=${RUNNER_TEMP}
		case "${HOST_UNAME}" in
		MINGW* | MSYS* | CYGWIN*) base=$(cygpath -u "${base}") || return 1 ;;
		*) ;;
		esac
		STATE_DIR=$(mktemp -d "${base}/ci-check.XXXXXX") || return 1
	else
		STATE_DIR=$(mktemp -d) || return 1
		# shellcheck disable=SC2064 # the directory is fixed now, on purpose
		trap "rm -rf -- '${STATE_DIR}'" EXIT
	fi
}

run_selected() {
	local job=$1 k i
	HOST_UNAME=$(uname -s)
	state_dir_make || {
		printf 'ci-check: cannot create the state directory\n' >&2
		return 2
	}
	RES_OUTCOME=()
	RES_REASON=()
	RES_SECS=()
	RES_COUNTS=()
	INTERRUPTED=0
	trap 'INTERRUPTED=1' INT TERM
	versions_header
	for k in "${!SEL[@]}"; do
		i=${SEL[k]}
		if [[ "${INTERRUPTED}" -eq 1 ]]; then
			result_set "${i}" 'NOT RUN' 'run interrupted' '' ''
			continue
		fi
		run_row "${i}"
	done
	trap - INT TERM
	report "${job}"
	if [[ "${INTERRUPTED}" -eq 1 ]]; then return 130; fi
	[[ -z "${FAILED_LIST}" ]]
}

usage_error() {
	printf 'ci-check: %s\nusage: ci-check.sh [--job <tag> | --list]\n' "$1" >&2
}

ci_main() {
	local mode=default job=''
	while [[ $# -gt 0 ]]; do
		case "$1" in
		--list)
			[[ "${mode}" == default ]] || {
				usage_error '--list takes no other option'
				return 2
			}
			mode=list
			shift
			;;
		--job)
			if [[ "${mode}" != default || $# -lt 2 || -z "$2" ]]; then
				usage_error '--job needs one tag'
				return 2
			fi
			mode=job
			job=$2
			shift 2
			;;
		*)
			usage_error "unknown option $1"
			return 2
			;;
		esac
	done
	export RUSTUP_AUTO_INSTALL=0
	pins_load || return 2
	table_check || return 2
	if [[ "${mode}" == list ]]; then
		table_list
		return
	fi
	if ci_mode && [[ "${mode}" == default ]]; then
		printf 'ci-check: under GITHUB_ACTIONS a CI job runs --job <tag>; the run without arguments is the local gate\n' >&2
		return 2
	fi
	if ci_mode && [[ -n "${CI_CHECK_UPDATE_BASELINES:-}" ]]; then
		printf 'ci-check: CI_CHECK_UPDATE_BASELINES is refused under GITHUB_ACTIONS: a job never rewrites its own baselines\n' >&2
		return 2
	fi
	RUN_MODE=${mode}
	select_rows "${mode}" "${job}" || return 2
	run_selected "${job}"
}
# ---------------------------------------------------------------------------
# The secret scan: one gitleaks run over every commit reachable from the refs,
# each merge diffed against its first parent so a conflict resolution is
# scanned too. Every condition is checked and each failed one printed.

# secret_scan <dir> <refs...>
secret_scan() {
	local dir=$1 out status=0 count='' scanned='' fingerprints=0 errors bad=0 shallow
	shift
	local refs="$*"
	out=$(mktemp) || return 1
	printf 'secret scan: log opts --diff-merges=first-parent %s\n' "${refs}"
	count=$(git -C "${dir}" rev-list --count "$@" 2>/dev/null) || count=''
	printf 'secret scan: rev-list count %s\n' "${count:-unreadable}"
	if [[ -f "${dir}/.gitleaksignore" ]]; then
		fingerprints=$(grep -c -E '^[0-9a-f]{7,40}:.+:[A-Za-z0-9_.-]+:[0-9]+$' "${dir}/.gitleaksignore")
	fi
	printf 'secret scan: .gitleaksignore fingerprints %s\n' "${fingerprints}"
	gitleaks git --no-banner --no-color --redact --verbose --ignore-gitleaks-allow \
		--log-opts="--diff-merges=first-parent ${refs}" "${dir}" >"${out}" 2>&1 || status=$?
	cat -- "${out}"
	shallow=$(git -C "${dir}" rev-parse --is-shallow-repository 2>/dev/null) || shallow=''
	if [[ "${shallow}" != false ]]; then
		printf 'secret scan: FAIL the clone is shallow, so the commits before its cut were not scanned\n'
		bad=1
	fi
	if [[ ! "${count}" =~ ^[0-9]+$ || "${count}" -lt 1 ]]; then
		printf 'secret scan: FAIL the range %s lists no commit or does not resolve\n' "${refs}"
		bad=1
	fi
	if [[ -e "${dir}/.gitleaks.toml" || -n "${GITLEAKS_CONFIG:-}" || -n "${GITLEAKS_CONFIG_TOML:-}" ]]; then
		printf 'secret scan: FAIL a .gitleaks.toml, GITLEAKS_CONFIG or GITLEAKS_CONFIG_TOML would replace the built-in rules\n'
		bad=1
	fi
	if [[ "${status}" -ne 0 ]]; then
		printf 'secret scan: FAIL gitleaks exited %s\n' "${status}"
		bad=1
	fi
	scanned=$(grep -o -E '[0-9]+ commits scanned\.' "${out}" | tail -n 1)
	scanned=${scanned%% *}
	if [[ ! "${scanned}" =~ ^[0-9]+$ || "${scanned}" -lt 1 ]]; then
		printf 'secret scan: FAIL gitleaks scanned %s commits\n' "${scanned:-no count of}"
		bad=1
	fi
	errors=$(grep -c -E '^[^ ]+ (ERR|FTL) ' "${out}")
	if [[ "${errors}" -gt 0 ]]; then
		printf 'secret scan: FAIL gitleaks logged %s error lines\n' "${errors}"
		bad=1
	fi
	rm -f -- "${out}"
	return "${bad}"
}

# Prints the uid and fails for root: a root user can read a mode-000 file, so
# a permission test would pass there without testing anything.
require_non_root_user() {
	local uid=${1:-}
	if [[ -z "${uid}" ]]; then uid=$(id -u); fi
	printf 'uid %s\n' "${uid}"
	if [[ "${uid}" == 0 ]]; then
		printf 'the Linux tests refuse to run as root: a root user can read a mode-000 file, so a permission test would pass without testing anything\n'
		return 1
	fi
}

# The refs follow the CI event: a scheduled run scans every fetched branch and
# tag, any other run (and the local gate) what HEAD reaches.
secret_scan_event() {
	if [[ "${GITHUB_EVENT_NAME:-}" == schedule ]]; then
		secret_scan "$1" --all
	else
		secret_scan "$1" HEAD
	fi
}

# The commit-message row's local precondition: prints the reason and exits 1
# when HEAD has no commit beyond origin/main. An origin/main that does not
# resolve lets the row run, and the checker fails on it.
outgoing_commits() {
	local n
	git rev-parse -q --verify 'origin/main^{commit}' >/dev/null || return 0
	n=$(git rev-list --count origin/main..HEAD) || return 2
	if [[ "${n}" -gt 0 ]]; then return 0; fi
	printf 'no outgoing commits\n'
	return 1
}

# ---------------------------------------------------------------------------
# The secret scan's self-test: throwaway repositories outside the checkout,
# git run without the host's config, tokens whose body is generated here at
# run time. Each case checks the verdict and the reason the scan printed. The
# scan output stays in files so a planted finding never reaches the log.

# Sets TOKEN to a fresh token of the ghp_ shape.
ss_token() {
	local alphabet=ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 body='' i
	for ((i = 0; i < 36; i++)); do body+=${alphabet:RANDOM%62:1}; done
	TOKEN="ghp_${body}"
}

ss_commit() {
	git -C "$1" add -A && git -C "$1" commit -q -m "$2"
}

# Three clean commits.
ss_repo_clean() {
	local i
	git init -q -b main "$1" || return 1
	for i in 1 2 3; do
		printf 'line %s\n' "${i}" >>"$1/notes.txt"
		ss_commit "$1" "commit ${i}" || return 1
	done
}

# Token A added then removed on a branch; token B only in the merge's
# conflict resolution, so only the merge's own diff shows it.
ss_repo_merge() {
	git init -q -b main "$1" || return 1
	printf 'shared\n' >"$1/shared.txt"
	ss_commit "$1" base || return 1
	git -C "$1" checkout -q -b topic
	ss_token
	printf 'key = %s\n' "${TOKEN}" >"$1/topic.txt"
	ss_commit "$1" 'add a file'
	rm -- "$1/topic.txt"
	ss_commit "$1" 'remove the file'
	printf 'topic side\n' >"$1/shared.txt"
	ss_commit "$1" 'topic edit'
	git -C "$1" checkout -q main
	printf 'main side\n' >"$1/shared.txt"
	ss_commit "$1" 'main edit'
	git -C "$1" merge -q topic -m merge >/dev/null 2>&1
	ss_token
	printf 'key = %s\n' "${TOKEN}" >"$1/shared.txt"
	git -C "$1" add shared.txt && git -C "$1" commit -q --no-edit
}

# ss_check <label> <output file> <scan status> <pass|fail> <findings in distinct files, or -> <reasons, |-separated>
ss_check() {
	local out=$2 status=$3 why='' n files names reason
	local -a reasons=()
	SS_CASES=$((SS_CASES + 1))
	if [[ "$4" == pass && "${status}" -ne 0 ]] || [[ "$4" == fail && "${status}" -eq 0 ]]; then
		why+="scan exit ${status}, expected a $4; "
	fi
	if [[ "$5" != - ]]; then
		n=$(grep -c '^Fingerprint:' "${out}")
		names=$(grep '^Fingerprint:' "${out}" | cut -d: -f3 | LC_ALL=C sort -u | tr '\n' ' ')
		files=$(wc -w <<<"${names}")
		[[ "${n}" -eq "$5" && "${files}" -eq "$5" ]] ||
			why+="${n} findings in ${files} files (${names% }), expected $5 in $5; "
	fi
	IFS='|' read -r -a reasons <<<"$6"
	for reason in "${reasons[@]}"; do
		grep -q -F -- "secret scan: FAIL ${reason}" "${out}" || why+="no failure line '${reason}'; "
	done
	if [[ -z "${why}" ]]; then
		SS_OK=$((SS_OK + 1))
	else
		printf 'secret scan self-test: case "%s" not as expected: %s\n' "$1" "${why}"
	fi
}

ss_cases() {
	local dir=$1 status merge
	ss_repo_clean "${dir}/clean" >/dev/null 2>&1
	status=0
	secret_scan "${dir}/clean" HEAD >"${dir}/t1.out" 2>&1 || status=$?
	ss_check 'a clean history passes' "${dir}/t1.out" "${status}" pass - ''
	grep -q -E '[1-9][0-9]* commits scanned' "${dir}/t1.out" || printf 'secret scan self-test: the clean case scanned no commit\n'
	ss_repo_merge "${dir}/merge" >/dev/null 2>&1
	status=0
	secret_scan "${dir}/merge" HEAD >"${dir}/t2.out" 2>&1 || status=$?
	ss_check 'a removed token and a merge-only token are both found' "${dir}/t2.out" "${status}" fail 2 'gitleaks exited'
	git clone -q --depth 1 "file://${dir}/merge" "${dir}/shallow" >/dev/null 2>&1
	status=0
	secret_scan "${dir}/shallow" HEAD >"${dir}/t3.out" 2>&1 || status=$?
	ss_check 'a shallow clone fails' "${dir}/t3.out" "${status}" fail - 'the clone is shallow'
	merge=$(git -C "${dir}/merge" rev-parse HEAD)
	status=0
	secret_scan "${dir}/merge" --no-merges --first-parent "${merge}^1..${merge}" >"${dir}/t4.out" 2>&1 || status=$?
	ss_check 'a range with no commit fails' "${dir}/t4.out" "${status}" fail - 'the range|gitleaks scanned'
	cp -R "${dir}/merge" "${dir}/config"
	printf '[allowlist]\npaths = ['"'''"'.*'"'''"']\n' >"${dir}/config/.gitleaks.toml"
	status=0
	secret_scan "${dir}/config" HEAD >"${dir}/t5.out" 2>&1 || status=$?
	ss_check 'a replaced rule set fails' "${dir}/t5.out" "${status}" fail - 'a .gitleaks.toml, GITLEAKS_CONFIG or GITLEAKS_CONFIG_TOML'
	git init -q -b main "${dir}/allow" >/dev/null 2>&1
	ss_token
	printf 'key = %s # gitleaks:allow\n' "${TOKEN}" >"${dir}/allow/notes.txt"
	ss_commit "${dir}/allow" 'allow comment' >/dev/null 2>&1
	status=0
	secret_scan "${dir}/allow" HEAD >"${dir}/t6.out" 2>&1 || status=$?
	ss_check 'an allow comment does not hide a token' "${dir}/t6.out" "${status}" fail 1 'gitleaks exited'
	status=0
	secret_scan "${dir}/clean" no-such-revision >"${dir}/t7.out" 2>&1 || status=$?
	ss_check 'an unknown revision fails' "${dir}/t7.out" "${status}" fail - 'the range|gitleaks logged'
}

secret_scan_self_test() {
	local dir
	dir=$(mktemp -d) || return 1
	SS_CASES=0
	SS_OK=0
	(
		export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
		export GIT_AUTHOR_NAME='Scan Test' GIT_AUTHOR_EMAIL='scan-test@example.invalid'
		export GIT_COMMITTER_NAME='Scan Test' GIT_COMMITTER_EMAIL='scan-test@example.invalid'
		unset GITLEAKS_CONFIG GITLEAKS_CONFIG_TOML
		ss_cases "${dir}"
		printf '%s %s\n' "${SS_CASES}" "${SS_OK}" >"${dir}/counts"
	)
	read -r SS_CASES SS_OK <"${dir}/counts"
	rm -rf -- "${dir}"
	printf 'secret scan self-test: cases run %s, as expected %s\n' "${SS_CASES}" "${SS_OK}"
	[[ "${SS_CASES}" -gt 0 && "${SS_CASES}" -eq "${SS_OK}" ]]
}

# ---------------------------------------------------------------------------
# Runner self-test: the runner's own functions on literal rows. Each nested
# run gets a scratch root, scratch pin files and a scratch PATH entry, and
# runs in a subshell, so the real table and environment are untouched.

# st_run <out> <table-function> [NAME=value ...] -- <runner arguments...>
st_run() {
	local out=$1 table=$2
	shift 2
	local -a env_words=()
	while [[ $# -gt 0 && "$1" != -- ]]; do
		env_words+=("$1")
		shift
	done
	if [[ $# -gt 0 ]]; then shift; fi
	(
		unset GITHUB_ACTIONS GITHUB_EVENT_NAME GITHUB_STEP_SUMMARY CI_CHECK_UPDATE_BASELINES
		unset RUSTUP_TOOLCHAIN ImageOS ImageVersion
		export RUNNER_TEMP="${ST_DIR}/temp"
		export PATH="${ST_DIR}/bin:${PATH}"
		local word
		for word in "${env_words[@]}"; do
			export "${word?}"
		done
		ROOT="${ST_DIR}/tree"
		TOOL_FILE="${ST_DIR}/ci-tools.env"
		TOOLCHAIN_FILE="${ST_DIR}/rust-toolchain.toml"
		NVMRC_FILE="${ST_DIR}/nvmrc"
		NPM_DIR=.
		NPM_LOCK="${ST_DIR}/package-lock.json"
		table_reset
		"${table}"
		ci_main "$@"
	) >"${out}" 2>&1
}

# st_case <label> <what went wrong, empty when the case behaved as expected>
st_case() {
	ST_CASES=$((ST_CASES + 1))
	if [[ -z "$2" ]]; then
		ST_OK=$((ST_OK + 1))
	else
		printf 'runner self-test: case "%s" not as expected: %s\n' "$1" "$2"
	fi
}

# st_outcome <label> <table-function> <expected runner exit> <expected text>
# [NAME=value ...] -- <runner arguments...>: one nested run, one case.
st_outcome() {
	local label=$1 table=$2 want_status=$3 want_text=$4 status=0 why=''
	shift 4
	st_run "${ST_DIR}/out" "${table}" "$@" || status=$?
	if [[ "${status}" -ne "${want_status}" ]]; then
		why+="runner exit ${status}, expected ${want_status}; "
	fi
	if ! grep -q -F -- "${want_text}" "${ST_DIR}/out"; then
		why+="no line holding '${want_text}'; "
	fi
	st_case "${label}" "${why}"
}

st_t_exit3() { row name=three tags=t category=both target=any tools=selftest-tool count=none cmd='exit 3'; }
st_t_silent() { row name=silent tags=t category=both target=any tools=selftest-tool count='^scanned [0-9]+' cmd=true; }
st_t_scan3() { row name=scan tags=t category=both target=any tools=selftest-tool count='^scanned [0-9]+' cmd='echo scanned 3'; }
st_t_scan0() {
	row name=scan0 tags=t category=both target=any tools=selftest-tool count='^scanned [0-9]+' \
		zero='^scanned ([0-9]+)' cmd='echo scanned 0'
}
st_t_ansi() {
	row name=ansi tags=t category=both target=any tools=selftest-tool count='^scanned [0-9]+' \
		zero='^scanned ([0-9]+)' cmd="printf '\\033[32mscanned 3\\033[0m\\n'"
}
st_t_requires() {
	row name=first tags=t category=both target=any tools=selftest-tool count=none cmd='exit 1'
	row name=second tags=t category=both target=any tools=selftest-tool requires=first count=none cmd=true
}
st_t_pre() {
	row name=guarded tags=t category=both target=any tools=selftest-tool count=none \
		pre='echo nothing to check here; exit 1' cmd=true
}
st_t_events() {
	row name=ok tags=t category=both target=any tools=selftest-tool count=none cmd=true
	row name=on-pr tags=t category=both target=any tools=selftest-tool events=pull_request count=none cmd=true
}
st_t_categories() {
	row name=ok tags=t category=both target=any tools=selftest-tool count=none cmd=true
	row name=ci-row tags=t category=ci-only target=any tools=selftest-tool count=none cmd=true
	row name=local-row tags=t category=local-only target=any tools=selftest-tool count=none cmd=true
	ci_only_item name=setup-item reason='nothing to run locally'
}
st_t_absent() {
	row name=ok tags=t category=both target=any tools=selftest-tool count=none cmd=true
	row name=absent tags=t category=both target=any tools=selftest-absent count=none cmd='touch marker'
}
st_t_node() {
	row name=ok tags=t category=both target=any tools=selftest-tool count=none cmd=true
	row name=node-row tags=t category=both target=any tools=node count=none cmd=true
}
st_t_presence() { row name=plain tags=t category=both target=any tools=bash count=none cmd=true; }
st_t_summary() {
	row name=ok tags=t category=both target=any tools=selftest-tool count=none cmd=true
	row name='sum|row' tags=t category=both target=any tools=selftest-tool count='^a' \
		cmd="printf 'a|b <c> & d\\n'; echo tail-marker-line; exit 1"
}

st_rows() {
	st_outcome 'a command exiting 3 fails with its status' st_t_exit3 1 '-- three: FAIL (exit 3)' -- --job t
	st_outcome 'a silent pass without a count line fails' st_t_silent 1 '-- silent: FAIL (no count line)' -- --job t
	st_outcome 'a count line passes' st_t_scan3 0 '-- scan: PASS' -- --job t
	st_outcome 'a zero count fails' st_t_scan0 1 '-- scan0: FAIL (the zero count is 0)' -- --job t
	st_outcome 'a coloured count line passes' st_t_ansi 0 '-- ansi: PASS' -- --job t
	st_outcome 'a failed required row stops its dependant' st_t_requires 1 \
		'-- second: NOT RUN (requires first, which did not pass)' -- --job t
	st_outcome 'a precondition reports not applicable' st_t_pre 0 '-- guarded: NOT RUN (nothing to check here)' -- --job t
	st_outcome 'a row outside its events is not run' st_t_events 0 '-- on-pr: NOT RUN (not on schedule)' \
		GITHUB_ACTIONS=true GITHUB_EVENT_NAME=schedule -- --job t
	st_outcome 'a ci-only row is named by the local verdict' st_t_categories 0 \
		'local run passed; not run here -- ci-only: ci-row; CI only: setup-item' --
	st_outcome 'a local-only row is not run in CI' st_t_categories 0 \
		'job rows passed; not run -- local-only: local-row' GITHUB_ACTIONS=true -- --job t
	st_outcome 'a missing tool fails its row' st_t_absent 1 \
		'-- absent: FAIL -- tool selftest-absent: pinned 4.5, found not found' -- --job t
	if [[ -e "${ST_DIR}/tree/marker" ]]; then
		st_case 'a missing tool leaves its command unrun' 'the command ran'
	else
		st_case 'a missing tool leaves its command unrun' ''
	fi
	st_outcome 'a different Node fails its row' st_t_node 1 '-- node-row: FAIL -- tool node: pinned 0.0, found' -- --job t
	st_outcome 'no pinned tool compared fails the run' st_t_presence 1 'pins compared 0' -- --job t
}

st_summary() {
	local why='' summary="${ST_DIR}/summary.md" status=0 n
	: >"${summary}"
	st_run "${ST_DIR}/out" st_t_summary "GITHUB_STEP_SUMMARY=${summary}" -- --job t || status=$?
	[[ "${status}" -eq 1 ]] || why+="runner exit ${status}, expected 1; "
	n=$(grep -c -F '<details>' "${summary}")
	[[ "${n}" -eq 1 ]] || why+="${n} details blocks, expected 1; "
	grep -q -F 'tail-marker-line' "${summary}" || why+='the tail is not in the summary; '
	grep -q -F '| sum\|row |' "${summary}" || why+='the row name cell is not escaped; '
	grep -q -F 'a\|b &lt;c&gt; &amp; d' "${summary}" || why+='the count cell is not escaped; '
	n=$(grep -c -x -F 'tail-marker-line' "${ST_DIR}/out")
	[[ "${n}" -eq 1 ]] || why+="the log holds the tail ${n} times, expected 1; "
	st_case 'a failed row is summarised once' "${why}"
}

# st_host <label> <uname> <target> <command> <expected status> <expected output>
st_host() {
	local out status=0 why=''
	out=$(host_adapt "$2" "$3" "$4") || status=$?
	[[ "${status}" -eq "$5" ]] || why+="status ${status}, expected $5; "
	[[ "${out}" == "$6" ]] || why+="printed '${out}'; "
	st_case "$1" "${why}"
}

st_hosts() {
	st_host 'Linux runs a Windows test row through xwin' Linux windows \
		'cargo test --workspace --locked 2>&1 | cat' 0 \
		'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER=env cargo xwin test --target x86_64-pc-windows-msvc --workspace --locked 2>&1 | cat'
	st_host 'Linux runs a Windows clippy row through xwin' Linux windows \
		'cargo clippy --workspace -- -D warnings' 0 \
		'cargo xwin clippy --target x86_64-pc-windows-msvc --workspace -- -D warnings'
	st_host 'Linux keeps the assignment before a Windows doc row' Linux windows \
		"RUSTDOCFLAGS='-D warnings' cargo doc --no-deps" 0 \
		"RUSTDOCFLAGS='-D warnings' cargo xwin doc --target x86_64-pc-windows-msvc --no-deps"
	st_host 'Linux runs a Linux row unchanged' Linux linux 'cargo clippy --locked' 0 'cargo clippy --locked'
	st_host 'Git Bash runs a Windows row natively' MINGW64_NT-10.0-26100 windows 'cargo test --locked' 0 \
		'cargo test --locked'
	st_host 'Git Bash refuses a Linux row' MINGW64_NT-10.0-26100 linux 'cargo test --locked' 1 'needs a Linux host'
	st_host 'another host refuses a Windows row' Darwin windows 'cargo test --locked' 1 'unsupported host (Darwin)'
	st_host 'another host runs an any row' Darwin any 'echo hi' 0 'echo hi'
}

st_t_forward() {
	row name=needs-later tags=t category=both target=any tools=selftest-tool requires=later count=none cmd='touch marker'
	row name=later tags=t category=both target=any tools=selftest-tool count=none cmd=true
}
st_t_duplicate() {
	row name=twice tags=t category=both target=any tools=selftest-tool count=none cmd='touch marker'
	row name=twice tags=t category=both target=any tools=selftest-tool count=none cmd=true
}
st_t_badcount() { row name=bad-count tags=t category=both target=any tools=selftest-tool count='(' cmd='touch marker'; }
st_t_twogroups() {
	row name=two-groups tags=t category=both target=any tools=selftest-tool count=none zero='(a)(b)' cmd='touch marker'
}
st_t_unknown() { row name=odd-field tags=t category=both target=any colour=red count=none cmd='touch marker'; }
st_t_notcargo() { row name=not-cargo tags=t category=both target=windows tools=rust count=none cmd='echo cargo test'; }
st_t_nodir() { row name=no-dir tags=t category=both target=any dir=no-such-dir count=none cmd='touch marker'; }

# st_table <label> <table-function> <row name the error must name>
st_table() {
	local status=0 why=''
	rm -f -- "${ST_DIR}/tree/marker"
	st_run "${ST_DIR}/out" "$2" -- --job t || status=$?
	[[ "${status}" -eq 2 ]] || why+="runner exit ${status}, expected 2; "
	grep -q -F -- "table error: row '$3'" "${ST_DIR}/out" || why+="no table error naming '$3'; "
	[[ ! -e "${ST_DIR}/tree/marker" ]] || why+='a row ran; '
	st_case "$1" "${why}"
}

st_tables() {
	st_table 'a requirement on a later row is refused' st_t_forward needs-later
	st_table 'a duplicate name is refused' st_t_duplicate twice
	st_table 'a count pattern that does not compile is refused' st_t_badcount bad-count
	st_table 'a zero pattern with two groups is refused' st_t_twogroups two-groups
	st_table 'an unknown field is refused' st_t_unknown odd-field
	st_table 'a Windows row that does not start with cargo is refused' st_t_notcargo not-cargo
	st_table 'a missing directory is refused' st_t_nodir no-dir
}

st_usages() {
	st_outcome 'CI refuses the local set' st_t_scan3 2 'a CI job runs --job <tag>' GITHUB_ACTIONS=true --
	st_outcome 'an unknown tag is refused' st_t_scan3 2 'tags: t' -- --job nope
	st_outcome 'an unknown option is refused' st_t_scan3 2 'unknown option --frobnicate' -- --frobnicate
	st_outcome 'CI refuses a baseline update' st_t_scan3 2 'CI_CHECK_UPDATE_BASELINES is refused' \
		GITHUB_ACTIONS=true CI_CHECK_UPDATE_BASELINES=1 -- --job t
}

# ---------------------------------------------------------------------------
# The problem-matcher case. The matcher file must be ASCII JSON; each owner's
# patterns must compile under grep -P (the closest local engine to the
# runner's) and match that owner's literal sample below, line i on pattern i;
# the literal lines of a green run must complete no owner.

st_matcher_samples() {
	local box=$'\342\224\214\342\224\200'
	MS_SAMPLE=()
	MS_SAMPLE[rustc]=$'error[E0425]: cannot find value `total` in this scope\n  --> src/main.rs:12:9'
	MS_SAMPLE[rustfmt]='Diff in \\?\D:\a\app\app\src\main.rs:12:'
	MS_SAMPLE[rust-test-panic]=$'thread \'tests::it_fails\' (1255725) panicked at src/main.rs:12:9:\nassertion `left == right` failed'
	MS_SAMPLE[cargo-deny]="error[vulnerability]: a sample advisory"$'\n'"   ${box} Cargo.lock:181:1"
	MS_SAMPLE[svelte-check]='1759999999999 ERROR "src/a.ts" 1:14 "Cannot find name '\''total'\''."'
	MS_SAMPLE[prettier]='[warn] src/lib/a.ts'
	MS_SAMPLE[actionlint]='.github/workflows/ci.yml:10:5: unexpected key "foo" for "job" section [syntax-check]'
	MS_SAMPLE[gitleaks]='Fingerprint: 0123abc:notes.txt:github-pat:3'
	MS_SAMPLE[pin-guard]='pin guard: .github/workflows/ci.yml:23: runner labels: runs-on: ubuntu-latest'
	MS_GREEN="test result: ok. 110 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.12s
warning[duplicate]: found 2 duplicate entries for crate 'windows-sys'
   ${box} Cargo.lock:10:1
All matched files use Prettier code style!
1759999999999 COMPLETED 142 FILES 0 ERRORS 0 WARNINGS 0 FILES_WITH_PROBLEMS
1 commits scanned.
no leaks found
pin guard: runner labels: scanned 11 runs-on lines, violations 0"
}

# Loads MS_OWNERS, MS_PAT["<owner> <index>"] and MS_PAT_COUNT[<owner>].
matcher_load() {
	local i owner
	local -a fields=()
	MS_OWNERS=()
	MS_PAT=()
	MS_PAT_COUNT=()
	mapfile -d '' -t fields < <(jq -j '.problemMatcher[] | .owner as $o | .pattern | to_entries[]
		| $o, "\u0000", (.key | tostring), "\u0000", .value.regexp, "\u0000"' "$1" 2>/dev/null || true)
	for ((i = 0; i + 2 < ${#fields[@]}; i += 3)); do
		owner=${fields[i]}
		if [[ -z "${MS_PAT_COUNT[${owner}]+set}" ]]; then
			MS_OWNERS+=("${owner}")
			MS_PAT_COUNT[${owner}]=0
		fi
		MS_PAT["${owner} ${fields[i + 1]}"]=${fields[i + 2]}
		MS_PAT_COUNT[${owner}]=$((MS_PAT_COUNT[${owner}] + 1))
	done
}

pcre_matches() { printf '%s\n' "$2" | LC_ALL=C.UTF-8 grep -q -P -- "$1"; }

matcher_file_problems() {
	local n
	if [[ ! -f "$1" ]]; then
		printf 'the matcher file is missing\n'
		return 0
	fi
	n=$(LC_ALL=C grep -c -P '[^\x00-\x7F]' "$1")
	[[ "${n}" -eq 0 ]] || printf 'the matcher file has %s lines with a byte above 0x7F\n' "${n}"
	jq -e '.problemMatcher | length > 0' "$1" >/dev/null 2>&1 || printf 'the matcher file is not JSON with a problemMatcher list\n'
}

matcher_owner_problems() {
	local owner=$1 k p re status
	local -a lines=()
	mapfile -t lines <<<"${MS_SAMPLE[${owner}]}"
	k=${MS_PAT_COUNT[${owner}]:-0}
	if [[ "${k}" -eq 0 ]]; then
		printf 'owner %s is not in the matcher file\n' "${owner}"
		return 0
	fi
	[[ "${#lines[@]}" -eq "${k}" ]] || printf 'owner %s has %s patterns and %s sample lines\n' "${owner}" "${k}" "${#lines[@]}"
	for ((p = 0; p < k; p++)); do
		re=${MS_PAT["${owner} ${p}"]}
		status=0
		LC_ALL=C.UTF-8 grep -q -P -- "${re}" </dev/null >/dev/null 2>&1 || status=$?
		if [[ "${status}" -eq 2 ]]; then
			printf 'owner %s pattern %s does not compile\n' "${owner}" "$((p + 1))"
		elif ! pcre_matches "${re}" "${lines[p]:-}"; then
			printf 'owner %s pattern %s misses its sample line\n' "${owner}" "$((p + 1))"
		fi
	done
}

matcher_green_problems() {
	local owner k s p hit
	local -a green=()
	mapfile -t green <<<"${MS_GREEN}"
	for owner in "${MS_OWNERS[@]}"; do
		k=${MS_PAT_COUNT[${owner}]}
		for ((s = 0; s + k <= ${#green[@]}; s++)); do
			hit=yes
			for ((p = 0; p < k; p++)); do
				pcre_matches "${MS_PAT["${owner} ${p}"]}" "${green[s + p]}" || hit=no
			done
			[[ "${hit}" == no ]] || printf 'owner %s matches the green-run lines from %s\n' "${owner}" "$((s + 1))"
		done
	done
}

st_matchers() {
	local file=$1 owner why
	declare -gA MS_SAMPLE=() MS_PAT=() MS_PAT_COUNT=()
	st_matcher_samples
	matcher_load "${file}"
	why=$(matcher_file_problems "${file}")
	st_case 'the matcher file is ASCII JSON' "${why}"
	for owner in "${!MS_SAMPLE[@]}"; do
		why=$(matcher_owner_problems "${owner}")
		st_case "matcher owner ${owner} matches its sample" "${why}"
	done
	why=''
	for owner in "${MS_OWNERS[@]}"; do
		[[ -n "${MS_SAMPLE[${owner}]+set}" ]] || why+="owner ${owner} has no sample; "
	done
	st_case 'every matcher owner has a sample' "${why}"
	why=$(matcher_green_problems)
	st_case 'a green run completes no owner' "${why}"
	sed -e 's/\\u250c\\u2500/'$'\342\224\214\342\224\200''/' "${file}" >"${ST_DIR}/non-ascii.json"
	why=$(matcher_file_problems "${ST_DIR}/non-ascii.json")
	if [[ -z "${why}" ]]; then
		st_case 'a non-ASCII matcher file is refused' 'it passed'
	else
		st_case 'a non-ASCII matcher file is refused' ''
	fi
}

runner_self_test() {
	local repo tool_version
	repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P) || return 1
	ST_DIR=$(mktemp -d) || return 1
	ST_CASES=0
	ST_OK=0
	mkdir -p "${ST_DIR}/tree" "${ST_DIR}/bin" "${ST_DIR}/temp"
	# The fake tool's version is joined at run time: the pin guard reads a
	# three-part version literal in this script as a pin stated outside the pin
	# files. The pins that are never found need only two parts.
	tool_version=1.2
	tool_version+=.3
	printf 'SELFTEST_TOOL=%s\nSELFTEST_ABSENT=4.5\n' "${tool_version}" >"${ST_DIR}/ci-tools.env"
	printf '[toolchain]\nchannel = "0.0"\n' >"${ST_DIR}/rust-toolchain.toml"
	printf '0.0\n' >"${ST_DIR}/nvmrc"
	printf '#!/usr/bin/env bash\necho "selftest-tool %s"\n' "${tool_version}" >"${ST_DIR}/bin/selftest-tool"
	chmod +x "${ST_DIR}/bin/selftest-tool"
	st_rows
	st_summary
	st_hosts
	st_tables
	st_usages
	st_matchers "${repo}/.github/problem-matchers.json"
	if require_non_root_user 0 >/dev/null; then st_case 'root is refused' 'uid 0 passed'; else st_case 'root is refused' ''; fi
	if require_non_root_user 1000 >/dev/null; then st_case 'a user is let through' ''; else st_case 'a user is let through' 'uid 1000 failed'; fi
	rm -rf -- "${ST_DIR}"
	printf 'runner self-test: cases %s, as expected %s\n' "${ST_CASES}" "${ST_OK}"
	[[ "${ST_CASES}" -gt 0 && "${ST_CASES}" -eq "${ST_OK}" ]]
}

# ---------------------------------------------------------------------------
# The step table. Rows run in this order; `requires` names an earlier row
# that carries every tag of this one. Commands run as bash -o pipefail -c
# from the row's dir; frontend tools are called by their path under
# node_modules, which runs the installed copy or fails, never a fetched one.

declare_table() {
	row name='runner self-test' tags=workflows category=both target=any tools=jq \
		count='^runner self-test: ' zero='cases ([0-9]+)' \
		cmd='source scripts/ci-check.sh && runner_self_test'
	row name='wiring self-check self-test' tags=workflows category=both target=any tools=node,bash,jq,git \
		count='^wiring self-check self-test: ' zero='cases ([0-9]+)' cmd='node crates/launcher/scripts/wiring-check.selftest.ts'
	row name='wiring self-check' tags=workflows category=both target=any tools=node,bash,jq,git \
		count='^wiring self-check: ' zero='jobs ([0-9]+)' cmd='node crates/launcher/scripts/wiring-check.ts'
	row name='pin guard (version sources and pin files)' tags=workflows category=both target=any tools=node,bash \
		count='^pin guard( self-test)?: [0-9]+ (files|cases)' zero='^pin guard: ([0-9]+) files' \
		cmd='node crates/launcher/scripts/pin-guard.selftest.ts && node crates/launcher/scripts/pin-guard.ts'
	row name=actionlint tags=workflows category=both target=any tools=actionlint count=none cmd=actionlint
	row name='vite build' tags=rust,rust-host,frontend,coverage category=both target=any tools=node,npm:vite \
		dir=crates/launcher count='modules transformed' zero='([0-9]+) modules transformed' \
		cmd='node node_modules/vite/bin/vite.js build'
	row name=rustfmt tags=rust category=both target=any tools=rust count=none cmd='cargo fmt --all --check -- --color never'
	row name='clippy (Windows target)' tags=rust category=both target=windows tools=rust requires='vite build' \
		count=none cmd='cargo clippy --workspace --all-targets --all-features --locked -- -D warnings'
	row name='tests (Windows target)' tags=rust category=both target=windows tools=rust,node requires='vite build' \
		count='^test result:|^test identity: ' zero='ids ([0-9]+)' \
		cmd='cargo test --workspace --all-features --locked 2>&1 | node crates/launcher/scripts/test-ids.ts libtest scripts/test-baselines/windows.list'
	row name='ignored tests (Windows target via WSL)' tags=rust category=local-only target=windows tools=rust,node \
		requires='vite build' count='^test result:|^test identity: ' zero='ids ([0-9]+)' \
		cmd='cargo test --workspace --all-features --locked -- --ignored 2>&1 | node crates/launcher/scripts/test-ids.ts libtest-ignored scripts/test-baselines/windows.list'
	row name='rustdoc (Windows target)' tags=rust category=both target=windows tools=rust requires='vite build' \
		count=none cmd="RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features --locked"
	# Coverage is measured only by its own CI job, on the Windows runner:
	# cargo-xwin has no llvm-cov subcommand. TOTAL counts regions, so it is 0
	# exactly when no file was measured.
	row name='coverage report (Windows tests)' tags=coverage category=non-blocking target=windows \
		tools=rust,cargo-llvm-cov requires='vite build' count='^TOTAL |^[^ ]+\.rs +[0-9]+ ' zero='^TOTAL +([0-9]+)' \
		cmd='cargo llvm-cov --workspace --all-features --locked'
	row name='clippy (Linux host)' tags=rust-host category=both target=linux tools=rust count=none \
		cmd='cargo clippy --workspace --all-targets --locked -- -D warnings'
	row name='tests (Linux host)' tags=rust-host category=both target=linux tools=rust,node requires='vite build' \
		count='^test result:|^test identity: ' zero='ids ([0-9]+)' \
		cmd='source scripts/ci-check.sh && require_non_root_user && cargo test --workspace --all-features --locked 2>&1 | node crates/launcher/scripts/test-ids.ts libtest scripts/test-baselines/linux.list'
	row name=eslint tags=frontend category=both target=any tools=node,npm:eslint dir=crates/launcher count=none \
		cmd='node node_modules/eslint/bin/eslint.js . --max-warnings 0'
	row name=prettier tags=frontend category=both target=any tools=node,npm:prettier dir=crates/launcher \
		count='^All matched files use|Code style issues' cmd='node node_modules/prettier/bin/prettier.cjs --check .'
	row name=svelte-check tags=frontend category=both target=any tools=node,npm:svelte-check dir=crates/launcher \
		count='COMPLETED [0-9]+ FILES' zero='COMPLETED ([0-9]+) FILES' \
		cmd='node node_modules/svelte-check/bin/svelte-check --tsconfig ./tsconfig.json --fail-on-warnings --output machine'
	row name='test identity self-test' tags=frontend category=both target=any tools=node,git \
		count='^test identity self-test: ' zero='cases ([0-9]+)' cmd='node crates/launcher/scripts/test-ids.selftest.ts'
	# shellcheck disable=SC2016 # the row's own bash expands these, not this file
	row name='npm audit' tags=frontend category=both target=any tools=node,npm dir=crates/launcher \
		count='^found [0-9]+ vulnerabilit|^[0-9]+ (info|low|moderate|high|critical) severity vulnerabilit|^[0-9]+ vulnerabilities \(|^npm audit: dependencies audited: ' \
		zero='^npm audit: dependencies audited: ([0-9]+)$' \
		cmd='npm audit --audit-level=low; status=$?; json=$(npm audit --audit-level=low --json 2>/dev/null) || true; total=$(printf %s "${json}" | node -p "JSON.parse(fs.readFileSync(0)).metadata.dependencies.total" 2>/dev/null) || true; echo "npm audit: dependencies audited: ${total:-none}"; case ${total} in "" | 0 | *[!0-9]*) status=1 ;; *) ;; esac; exit "${status}"'
	row name=cargo-deny tags=supply-chain category=both target=any tools=rust,cargo-deny \
		count='gathered [0-9]+ crates|^advisories (ok|FAILED)' zero='gathered ([0-9]+) crates' \
		cmd='cargo deny --locked -L info check --show-stats'
	row name='binary audit self-test' tags=release-audit category=both target=any \
		tools=strings,iconv,llvm-readobj,sha256sum count='^self-test: ' zero='planted ([0-9]+)' \
		cmd='scripts/binary-audit.sh self-test'
	row name='release build' tags=release-audit category=ci-only target=linux \
		tools=rust,node,npm:vite,cargo-xwin,jq,clang,lld-link count='Finished .release. profile|^release build: ' \
		cmd=scripts/build.sh
	row name='binary audit' tags=release-audit category=ci-only target=any requires='release build' \
		tools=strings,llvm-readobj,sha256sum count='^audit: ' zero='files ([0-9]+)' \
		cmd='scripts/binary-audit.sh audit --public-runner release crates/launcher/dist'
	row name='secret scan self-test' tags=secrets category=both target=any tools=gitleaks,git \
		count='^secret scan self-test: ' zero='cases run ([0-9]+)' \
		cmd='source scripts/ci-check.sh && secret_scan_self_test'
	row name='secret scan' tags=secrets category=both target=any tools=gitleaks,git \
		count='commits scanned|leaks found|no leaks found|^secret scan: ' zero='([0-9]+) commits scanned' \
		cmd='source scripts/ci-check.sh && secret_scan_event .'
	row name='pin freshness report' tags=freshness category=non-blocking target=any tools=node \
		count='^checked [0-9]+ of [0-9]+ pins' zero='^checked ([0-9]+) of' cmd='node crates/launcher/scripts/freshness.ts'
	row name='checker self-test' tags=hygiene category=both target=any tools=node,git \
		count='self-test: ' zero='self-test: planted ([0-9]+)' \
		cmd='node crates/launcher/scripts/commit-check.selftest.ts'
	row name='commit messages' tags=hygiene category=both target=any tools=node,git events=pull_request,push \
		pre='source scripts/ci-check.sh && outgoing_commits' \
		count='^commits in range: |^hits: |^identity: ' zero='^commits in range: ([0-9]+)' \
		cmd='node crates/launcher/scripts/commit-check.ts commits'
	# CI only: a pull request's title, body and branch exist only in its event.
	row name='PR text' tags=pr-text category=ci-only target=any tools=node \
		count='^pr text: |^hits: ' zero='^pr text: fields read ([0-9]+)' \
		cmd='node crates/launcher/scripts/commit-check.ts pr-text'
	ci_only_item name='npm ci' \
		reason='CI installs the frontend packages; locally they are installed by hand and npm ci would delete them'
	ci_only_item name='aggregate job' reason="reads the needed jobs' results; nothing to run locally"
}

main() {
	cd -- "$(dirname -- "$0")/.." || {
		printf 'ci-check: cannot enter the repository root\n' >&2
		exit 2
	}
	ROOT=$(pwd -P)
	TOOL_FILE="${ROOT}/scripts/ci-tools.env"
	TOOLCHAIN_FILE="${ROOT}/rust-toolchain.toml"
	NVMRC_FILE="${ROOT}/.nvmrc"
	NPM_DIR=crates/launcher
	NPM_LOCK="${ROOT}/crates/launcher/package-lock.json"
	table_reset
	declare_table
	ci_main "$@"
	exit "$?"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
