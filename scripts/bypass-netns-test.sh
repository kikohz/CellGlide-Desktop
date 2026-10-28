#!/bin/bash
# Runs the bypass tests inside a throwaway network namespace with two dummy uplinks,
# so they never touch the host's routes. Needs root. Usage: bypass-netns-test.sh <privileged test binary>
set -eu
NS=cgbypass
BIN=$1
ip netns del "$NS" 2>/dev/null || true
ip netns add "$NS"
trap 'ip netns del "$NS"' EXIT
ip -n "$NS" link set lo up
for i in 1 2; do
    ip -n "$NS" link add "up$i" type dummy
    ip -n "$NS" addr add "10.9$i.0.2/24" dev "up$i"
    ip -n "$NS" link set "up$i" up
done
ip -n "$NS" route add default via 10.91.0.1 dev up1 metric 100
CELLGLIDE_NETNS=1 ip netns exec "$NS" "$BIN" --ignored --test-threads=1 bypass_
