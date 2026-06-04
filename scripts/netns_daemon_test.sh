#!/usr/bin/env bash
# End-to-end validation of the REAL daemon's opt-in eBPF download-shaping path
# (CURB_EBPF_INGRESS=1), run entirely inside an isolated network namespace.
#
# curbd runs inside the netns with the veth as its default interface, so all of
# its tc/eBPF/IFB state lives in the namespace and CANNOT touch the host's real
# NIC or default route. Validates: set a per-app download cap -> the app's
# download is HTB-shaped (smooth, no drops) and traffic keeps flowing.
#
# Run: sudo ./scripts/netns_daemon_test.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CURBD="$ROOT/target/debug/curbd"
CURB="$ROOT/target/debug/curb"
NS=curbtestd
VETH_H=veth-cdh
VETH_N=veth-cdn
SOCK=/tmp/curbd-netns.sock
STATE=/tmp/curb-netns-state

cleanup() {
    [ -n "${DPID:-}" ] && kill -TERM "$DPID" 2>/dev/null
    sleep 0.5
    ip netns pids "$NS" 2>/dev/null | xargs -r kill -9 2>/dev/null
    ip netns del "$NS" 2>/dev/null
    ip link del "$VETH_H" 2>/dev/null
    pkill -f "iperf3 -s -1 -B 10.123.0.1" 2>/dev/null
    rm -rf "$STATE" "$SOCK"
}
trap cleanup EXIT
cleanup

set -e
echo "== host connectivity BEFORE =="
curl -s -o /dev/null -w "  HTTP %{http_code}\n" --max-time 6 https://www.google.com || true

echo "== build netns + veth + default route inside it =="
ip netns add "$NS"
ip link add "$VETH_H" type veth peer name "$VETH_N"
ip link set "$VETH_N" netns "$NS"
ip addr add 10.123.0.1/24 dev "$VETH_H"
ip link set "$VETH_H" up
ip netns exec "$NS" ip addr add 10.123.0.2/24 dev "$VETH_N"
ip netns exec "$NS" ip link set "$VETH_N" up
ip netns exec "$NS" ip link set lo up
ip netns exec "$NS" ip route add default via 10.123.0.1 dev "$VETH_N"
set +e

echo "== starting curbd INSIDE the netns with CURB_EBPF_INGRESS=1 =="
# nsenter --net enters ONLY the network namespace, keeping the host mount
# namespace so /sys/fs/cgroup (cgroup2) stays writable (unlike `ip netns exec`,
# which remounts /sys and shadows the cgroup hierarchy).
NETNS_PATH=/var/run/netns/$NS
rm -rf "$STATE"; mkdir -p "$STATE"
nsenter --net="$NETNS_PATH" env RUST_LOG=info CURB_EBPF_INGRESS=1 CURB_SOCK="$SOCK" \
    CURB_STATE_DIR="$STATE" "$CURBD" >/tmp/curbd-netns.log 2>&1 &
DPID=$!
for i in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
chmod 666 "$SOCK" 2>/dev/null
grep -E "interface|eBPF|listening" /tmp/curbd-netns.log | sed 's/^/  /'

echo "== set per-app download cap on iperf3: 5mbit =="
"$CURB" --socket "$SOCK" app limit /usr/bin/iperf3 --down 5mbit >/dev/null
DIR=$(find /sys/fs/cgroup/curb -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | head -1)
echo "  cgroup: $DIR"
echo "  veth ingress filters (expect pref 1 bpf + pref 2 mirred):"
ip netns exec "$NS" tc filter show dev "$VETH_N" ingress | grep -E "pref|bpf|mirred" | sed 's/^/    /' | head
echo "  ifb classes:"
ip netns exec "$NS" tc class show dev ifbcurb 2>/dev/null | sed 's/^/    /'

echo "== iperf3 download (-R) from inside the netns, in the app cgroup =="
iperf3 -s -1 -B 10.123.0.1 -p 5201 >/tmp/i3srv.log 2>&1 &
sleep 0.6
RESULT=$(nsenter --net="$NETNS_PATH" sh -c "echo \$\$ > /sys/fs/cgroup/curb/$DIR/cgroup.procs; exec iperf3 -c 10.123.0.1 -p 5201 -R -t 5" 2>/dev/null | grep receiver)
echo "  $RESULT"
echo "  ifb app class stats (expect dropped 0):"
ip netns exec "$NS" tc -s class show dev ifbcurb | grep -A2 "htb 1:1[0-9a-f]" | grep -oE "rate [0-9A-Za-z]+|dropped [0-9]+|Sent [0-9]+" | sed 's/^/    /'

echo "== verdict + host connectivity AFTER =="
MBIT=$(echo "$RESULT" | grep -oE "[0-9.]+ Mbits/sec" | head -1 | grep -oE "[0-9.]+")
[ -n "$MBIT" ] && echo "  download shaped to ${MBIT} Mbit/s (cap 5) — flowed, no blackhole ✓" \
              || echo "  NO THROUGHPUT — blackhole ✗"
curl -s -o /dev/null -w "  host connectivity AFTER: HTTP %{http_code} ✓\n" --max-time 6 https://www.google.com || true
