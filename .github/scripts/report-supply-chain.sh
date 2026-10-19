#!/usr/bin/env bash
set -uo pipefail

check="${1:?check name required}"
status="${2:-}"
log_file="${3:-}"
summary="${GITHUB_STEP_SUMMARY:-}"

if [ -z "$summary" ]; then
    summary=/dev/null
fi

append_summary() {
    printf '%s\n' "$1" >>"$summary"
}

sanitize_summary_line() {
    local line="$1"
    line="${line//$'\r'/}"
    line="${line//::/%3A%3A}"
    printf '%s' "$line"
}

append_indented_stream() {
    local limit="$1"
    local shown=0
    while IFS= read -r line; do
        if [ "$shown" -ge "$limit" ]; then
            printf '    ... (truncated)\n' >>"$summary"
            return
        fi
        printf '    %s\n' "$(sanitize_summary_line "$line")" >>"$summary"
        shown=$((shown + 1))
    done
}

escape_command_data() {
    local value="$1"
    value="${value//%/%25}"
    value="${value//$'\r'/%0D}"
    value="${value//$'\n'/%0A}"
    value="${value//::/%3A%3A}"
    printf '%s' "$value"
}

escape_command_property() {
    local value
    value="$(escape_command_data "$1")"
    value="${value//:/%3A}"
    value="${value//,/%2C}"
    printf '%s' "$value"
}

emit_error_annotation() {
    local title="$1"
    local message="$2"
    printf '::error title=%s::%s\n' \
        "$(escape_command_property "$title")" \
        "$(escape_command_data "$message")"
}

if [ -z "$status" ]; then
    append_summary "### ${check}: did not report a status"
    append_summary ""
    append_summary "The check did not run to completion, so no exit status is available. An earlier setup step may have failed; inspect the job log and re-run the job."
    emit_error_annotation "${check} did not report a status" "Inspect the job log for a failed setup step."
    exit 0
fi

if ! [[ "$status" =~ ^[0-9]+$ ]]; then
    append_summary "### ${check}: unknown result"
    append_summary ""
    append_summary "Reported status: ${status}"
    emit_error_annotation "${check} returned an unknown status" "Reported status: ${status}"
    exit 0
fi

if [ "$status" -eq 0 ]; then
    append_summary "### ${check}: passed"
    exit 0
fi

append_summary "### ${check}: failed (exit ${status})"
append_summary ""

findings=""
if [ -n "$log_file" ] && [ -s "$log_file" ]; then
    findings="$(
        grep -E '^(error(\[[^]]+\])?:|warning(\[[^]]+\])?:|ID: |Package: |Severity: |Title: |Solution: )|^[[:space:]]+(crate|package|dependency|id):' "$log_file" ||
            true
    )"
fi

if [ -n "$findings" ]; then
    append_summary "**Key failure lines**"
    append_summary ""
    printf '%s\n' "$findings" | append_indented_stream 20
    append_summary ""
else
    append_summary "No structured finding lines were detected in the check output."
    append_summary ""
fi

if [ -n "$log_file" ] && [ -s "$log_file" ]; then
    total_lines="$(wc -l <"$log_file" | tr -d ' ')"
    append_summary "<details><summary>Full check output (${total_lines} lines)</summary>"
    append_summary ""
    sed -n '1,200p' "$log_file" | append_indented_stream 200
    append_summary ""
    if [ "$total_lines" -gt 200 ]; then
        append_summary "Output truncated at 200 lines; see the step log for the rest."
        append_summary ""
    fi
    append_summary "</details>"
    append_summary ""
fi

append_summary "Remediation: review the dependency or advisory named above, update or replace the affected crate, or document and narrowly scope any approved exception. Do not disable the check to make the job pass."

first_finding="$(printf '%s\n' "$findings" | sed -n '1p')"
if [ -n "$first_finding" ]; then
    emit_error_annotation "${check} failed" "Exit status ${status}. First finding: ${first_finding}"
else
    emit_error_annotation "${check} failed" "Exit status ${status}. See the job summary and step log for details."
fi
