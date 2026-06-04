#!/usr/bin/env bash
# Validate the SAFE per-app ingress download-shaping mechanism in an isolated
# network namespace: `clsact ingress + (priority classification) + mirred egress
# redirect dev ifb + HTB on the ifb`. This is the reinjection-correct path
# (mirred, as P2 uses) — NOT bpf_redirect, which blackholed the live host.
#
# Everything runs on a private veth subnet inside a throwaway netns and is torn
# down on exit, so it CANNOT affect the host's real interface or default route.
#
# Run: sudo ./scripts/netns_ingress_test.sh
set -uo pipefail

NS=curbtest
VETH_H=veth-curbh
VETH_N=veth-curbn
IFB=ifb-curbt
RATE_MBIT=5

cleanup() {
    ip netns del "$NS" 2>/dev/null
    ip link del "$VETH_H" 2>/dev/null
    pkill -f "iperf3 -s -1 -B 10.123.0.1" 2>/dev/null
}
trap cleanup EXIT
cleanup

set -e
echo "== creating isolated netns ($NS) with veth + ifb =="
ip netns add "$NS"
ip link add "$VETH_H" type veth peer name "$VETH_N"
ip link set "$VETH_N" netns "$NS"
ip addr add 10.123.0.1/24 dev "$VETH_H"
ip link set "$VETH_H" up
ip netns exec "$NS" ip addr add 10.123.0.2/24 dev "$VETH_N"
ip netns exec "$NS" ip link set "$VETH_N" up
ip netns exec "$NS" ip link set lo up

echo "== installing ingress shaping inside the netns =="
ip netns exec "$NS" ip link add "$IFB" type ifb
ip netns exec "$NS" ip link set "$IFB" up
# clsact ingress on the netns interface: set the HTB class via priority, then
# redirect (with mirred — reinjects correctly) to the ifb device for shaping.
ip netns exec "$NS" tc qdisc add dev "$VETH_N" clsact
ip netns exec "$NS" tc filter add dev "$VETH_N" ingress matchall \
    action skbedit priority 0x10010 \
    action mirred egress redirect dev "$IFB"
# ifb HTB: default class at line rate, app class 1:10 capped.
ip netns exec "$NS" tc qdisc add dev "$IFB" root handle 1: htb default 1
ip netns exec "$NS" tc class add dev "$IFB" parent 1: classid 1:1 htb rate 1000mbit
ip netns exec "$NS" tc class add dev "$IFB" parent 1: classid 1:10 \
    htb rate ${RATE_MBIT}mbit ceil ${RATE_MBIT}mbit
set +e

echo "== running iperf3 (-R = download INTO the netns, the shaped direction) =="
iperf3 -s -1 -B 10.123.0.1 -p 5201 >/tmp/i3srv.log 2>&1 &
sleep 0.6
RESULT=$(ip netns exec "$NS" iperf3 -c 10.123.0.1 -p 5201 -R -t 5 2>/dev/null | grep receiver)
echo "  $RESULT"

echo "== ifb class stats (expect dropped 0 = smooth shaping, not blackhole) =="
ip netns exec "$NS" tc -s class show dev "$IFB" | grep -A2 "class htb 1:10" | sed 's/^/  /'

echo "== verdict =="
MBIT=$(echo "$RESULT" | grep -oE "[0-9.]+ Mbits/sec" | head -1 | grep -oE "[0-9.]+")
if [ -n "$MBIT" ]; then
    echo "  download shaped to ${MBIT} Mbit/s (cap ${RATE_MBIT} Mbit) — traffic flowed, NO blackhole ✓"
else
    echo "  NO THROUGHPUT — would indicate a blackhole (mechanism unsafe) ✗"
fi
