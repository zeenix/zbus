#!/bin/sh
# Builds the GeoClue2 fixtures under the `size` profile on each runtime, prints their stripped
# sizes, and runs the pair once on a private session bus to prove the binaries work. Run from
# the repository root.
#
# This tree's raw sizes are also written to target/size/sizes, one "<binary> <runtime> <bytes>"
# line per fixture. With a git revision as $1 (BASE), the two trees' sizes are compared instead
# of just this one's being printed, and a binary that grew by more than $SIZE_LIMIT_PERCENT
# (default 5) percent fails the script.
#
# BINARY_SIZE_MEASURE_ONLY=1 builds and measures only, skipping the smoke test and the table.
# This is what BASE is run with when comparing: running BASE's own binaries proves nothing about
# the tree being compared against it, and only adds a way for that comparison to fail on BASE's
# account.
set -eu

size_limit_percent=${SIZE_LIMIT_PERCENT:-5}

sizes=""
table="| binary | builtin | tokio |
|---|---|---|"
for bin in service client; do
    row="| $bin |"
    for runtime in builtin tokio; do
        cargo build --locked --profile size -p "geoclue_${bin}_fixture" \
            --no-default-features --features "$runtime" >/dev/null
        size=$(stat -c %s "target/size/geoclue_${bin}_fixture")
        row="$row $((size / 1024)) KiB |"
        cp "target/size/geoclue_${bin}_fixture" "target/size/geoclue_${bin}_${runtime}"
        sizes="$sizes$bin $runtime $size
"
    done
    table="$table
$row"
done
printf '%s' "$sizes" >target/size/sizes

if [ "${BINARY_SIZE_MEASURE_ONLY:-0}" != 1 ]; then
    for runtime in builtin tokio; do
        # Capture the client's output instead of piping it straight to grep, so that the exit
        # status this loop sees (via `set -e`) is the client's own, not grep's: a client that
        # prints the latitude and then fails would otherwise still pass. The client is retried
        # rather than given a fixed wait, because how long the service takes to claim its bus
        # name is the machine's business: a loaded runner can take longer than any wait worth
        # writing down. Each attempt runs under `timeout`, and a timed-out attempt (124) ends
        # the retries at once instead of being retried: a client that hangs waiting for a
        # reply or a signal is not a service that has yet to claim its name. Only the last
        # attempt's output is shown, so that the attempts made before the name is there say
        # nothing and a run that gives up says why.
        if ! output=$(dbus-run-session -- sh -c "
            target/size/geoclue_service_$runtime &
            service=\$!
            attempt=1
            while :; do
                client=\$(timeout 10s target/size/geoclue_client_$runtime 2>&1)
                status=\$?
                if [ \$status -eq 0 ]; then
                    break
                fi
                if [ \$status -eq 124 ]; then
                    client=\"the client hung past the 10s deadline: \$client\"
                    break
                fi
                if [ \$attempt -ge 20 ]; then
                    break
                fi
                attempt=\$((attempt + 1))
                sleep 0.25
            done
            kill \"\$service\" 2>/dev/null || true
            printf '%s\n' \"\$client\"
            exit \$status
        "); then
            printf 'the %s pair could not talk over a private session bus:\n%s\n' \
                "$runtime" "$output" >&2
            exit 1
        fi
        printf '%s\n' "$output" | grep -q '^Latitude: 59\.3293$'
    done
fi

if [ $# -eq 0 ]; then
    if [ "${BINARY_SIZE_MEASURE_ONLY:-0}" != 1 ]; then
        echo "$table"
    fi
    exit 0
fi
base=$1

# Measure BASE the same way, in a worktree of its own so this tree's checkout (and target/)
# is left alone. A worktree, not a plain checkout, because it shares this clone's objects
# instead of needing its own fetch.
worktree_dir=$(mktemp -d)
# shellcheck disable=SC2329 # invoked indirectly, via the trap below
cleanup() {
    git worktree remove --force "$worktree_dir"
}
trap cleanup EXIT
git worktree add --detach "$worktree_dir" "$base" >/dev/null

if [ ! -f "$worktree_dir/CI/binary-size.sh" ]; then
    echo "the base has no fixtures to compare against"
    echo "$table"
    exit 0
fi

# BASE is measured with BINARY_SIZE_MEASURE_ONLY, so its own script only builds and measures:
# neither its table nor its smoke test says anything about the tree being compared against it.
# Its output goes to this script's stderr, so that a base that fails to measure can be diagnosed
# without its table landing in ours.
if ! (cd "$worktree_dir" && BINARY_SIZE_MEASURE_ONLY=1 CI/binary-size.sh) >&2; then
    echo "the base's CI/binary-size.sh failed to measure its fixtures" >&2
    exit 1
fi

if [ ! -f "$worktree_dir/target/size/sizes" ]; then
    echo "the base's CI/binary-size.sh left no target/size/sizes to compare against" >&2
    exit 1
fi

compare_table="| binary | runtime | base | this tree | change |
|---|---|---|---|---|"
failures=""
for bin in service client; do
    for runtime in builtin tokio; do
        base_bytes=$(awk -v b="$bin" -v r="$runtime" '$1 == b && $2 == r { print $3 }' \
            "$worktree_dir/target/size/sizes")
        this_bytes=$(awk -v b="$bin" -v r="$runtime" '$1 == b && $2 == r { print $3 }' \
            target/size/sizes)

        if [ -z "$base_bytes" ] || [ -z "$this_bytes" ]; then
            base_col="-"
            this_col="-"
            [ -n "$base_bytes" ] && base_col="$((base_bytes / 1024)) KiB"
            [ -n "$this_bytes" ] && this_col="$((this_bytes / 1024)) KiB"
            compare_table="$compare_table
| $bin | $runtime | $base_col | $this_col | - |"
            continue
        fi

        base_col="$((base_bytes / 1024)) KiB"
        this_col="$((this_bytes / 1024)) KiB"
        change=$(awk -v b="$base_bytes" -v t="$this_bytes" \
            'BEGIN { printf "%.1f", (t - b) / b * 100 }')
        compare_table="$compare_table
| $bin | $runtime | $base_col | $this_col | $change% |"

        # Decided on the raw byte counts, not the rounded "$change" percentage above: a growth
        # of 5.04 % displays as "5.0%" and must still fail a 5 % limit.
        grew=$(awk -v b="$base_bytes" -v t="$this_bytes" -v lim="$size_limit_percent" \
            'BEGIN { print ((t - b) * 100 > lim * b) }')
        if [ "$grew" -eq 1 ]; then
            failures="$failures$bin $runtime $base_bytes $this_bytes $change
"
        fi
    done
done

echo "$compare_table"

if [ -z "$failures" ]; then
    exit 0
fi
echo "$failures" | while IFS=' ' read -r bin runtime base_bytes this_bytes change; do
    [ -z "$bin" ] && continue
    message="$bin ($runtime) grew from $((base_bytes / 1024)) KiB to \
$((this_bytes / 1024)) KiB ($change%), over the ${size_limit_percent}% limit"
    # On stderr either way, so that it stays out of the table a caller may be saving; the Actions
    # runner reads workflow commands from both streams.
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        echo "::error title=Binary size::$message" >&2
    else
        echo "$message" >&2
    fi
done
exit 1
