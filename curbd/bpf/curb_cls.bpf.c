// CURB eBPF traffic classifier.
//
// Steers each packet to a per-app HTB class by writing skb->priority to the
// class handle; the HTB qdisc then *shapes* (queues) rather than polices (drops).
//
//  * curb_egress  — egress hook. Reads the sending socket's cgroup with
//                   bpf_skb_cgroup_id(), sets the class, and records the flow's
//                   reverse 4-tuple -> classid so the matching inbound packets
//                   can be classified without a socket lookup.
//  * curb_ingress — ingress hook (socket not yet looked up). Looks the packet's
//                   4-tuple up in the flow map, sets the class, and redirects to
//                   the IFB device where the HTB qdisc shapes it.
//
// This avoids bpf_sk_lookup / bpf_sk_cgroup_id (not available to tc programs);
// only bpf_skb_cgroup_id, map ops, and bpf_redirect are used.
//
// Userspace (curbd, via Aya) fills cgroup_classid (cgroup_id -> classid) and
// sets config[0] to the IFB ifindex (0 disables ingress redirect).

#include <linux/bpf.h>
#include <linux/pkt_cls.h>
#include <linux/if_ether.h>
#include <linux/ip.h>
#include <linux/in.h>
#include <linux/tcp.h>
#include <linux/udp.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_endian.h>

// A connection's identifying 4-tuple (+proto).
struct flow_key {
    __u32 saddr;
    __u32 daddr;
    __u16 sport;
    __u16 dport;
    __u8 proto;
    __u8 pad[3];
};

// Parsed packet fields.
struct pkt5 {
    __u32 saddr;
    __u32 daddr;
    __u16 sport;
    __u16 dport;
    __u8 proto;
};

// cgroup v2 id -> tc class handle (e.g. 0x00010010 for class 1:16).
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, __u64);
    __type(value, __u32);
} cgroup_classid SEC(".maps");

// Reverse-flow -> classid, populated on egress for inbound classification.
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 65536);
    __type(key, struct flow_key);
    __type(value, __u32);
} flow_classid SEC(".maps");

// config[0] = IFB ifindex for ingress redirect (0 = ingress shaping disabled).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u32);
} config SEC(".maps");

// Parse IPv4 TCP/UDP (no IP options) into `o`. Returns 0 on success.
static __always_inline int parse_pkt(struct __sk_buff *skb, struct pkt5 *o)
{
    void *data = (void *)(long)skb->data;
    void *data_end = (void *)(long)skb->data_end;

    struct ethhdr *eth = data;
    if ((void *)(eth + 1) > data_end)
        return -1;
    if (eth->h_proto != bpf_htons(ETH_P_IP))
        return -1;

    struct iphdr *ip = (void *)(eth + 1);
    if ((void *)(ip + 1) > data_end)
        return -1;
    if (ip->ihl != 5)
        return -1;

    o->saddr = ip->saddr;
    o->daddr = ip->daddr;
    o->proto = ip->protocol;

    void *l4 = (void *)(ip + 1);
    if (ip->protocol == IPPROTO_TCP) {
        struct tcphdr *t = l4;
        if ((void *)(t + 1) > data_end)
            return -1;
        o->sport = t->source;
        o->dport = t->dest;
    } else if (ip->protocol == IPPROTO_UDP) {
        struct udphdr *u = l4;
        if ((void *)(u + 1) > data_end)
            return -1;
        o->sport = u->source;
        o->dport = u->dest;
    } else {
        return -1;
    }
    return 0;
}

// Egress: classify by the sending socket's cgroup (new connections), falling
// back to the flow map (existing connections, seeded by userspace from /proc).
// On a hit, record the reverse flow so the inbound direction matches too.
SEC("classifier")
int curb_egress(struct __sk_buff *skb)
{
    __u32 classid = 0;
    __u64 cgid = bpf_skb_cgroup_id(skb);
    if (cgid) {
        __u32 *c = bpf_map_lookup_elem(&cgroup_classid, &cgid);
        if (c)
            classid = *c;
    }

    struct pkt5 p = {};
    int parsed = parse_pkt(skb, &p);
    // Existing-connection fallback: look this outbound flow up directly.
    if (!classid && parsed == 0) {
        struct flow_key fk = {
            .saddr = p.saddr,
            .daddr = p.daddr,
            .sport = p.sport,
            .dport = p.dport,
            .proto = p.proto,
        };
        __u32 *c = bpf_map_lookup_elem(&flow_classid, &fk);
        if (c)
            classid = *c;
    }
    if (!classid)
        return TC_ACT_OK;

    skb->priority = classid;
    if (parsed == 0) {
        // Reverse the tuple: an inbound reply has src/dst and ports swapped.
        struct flow_key rk = {
            .saddr = p.daddr,
            .daddr = p.saddr,
            .sport = p.dport,
            .dport = p.sport,
            .proto = p.proto,
        };
        bpf_map_update_elem(&flow_classid, &rk, &classid, BPF_ANY);
    }
    return TC_ACT_OK;
}

// Ingress: classify by the recorded flow, then redirect to the IFB device for
// shaping (the HTB qdisc on IFB reads skb->priority).
SEC("classifier")
int curb_ingress(struct __sk_buff *skb)
{
    __u32 zero = 0;
    __u32 *ifbp = bpf_map_lookup_elem(&config, &zero);
    __u32 ifb = ifbp ? *ifbp : 0;
    if (ifb == 0)
        return TC_ACT_OK; // ingress shaping disabled

    struct pkt5 p;
    if (parse_pkt(skb, &p) == 0) {
        struct flow_key k = {
            .saddr = p.saddr,
            .daddr = p.daddr,
            .sport = p.sport,
            .dport = p.dport,
            .proto = p.proto,
        };
        __u32 *classid = bpf_map_lookup_elem(&flow_classid, &k);
        if (classid)
            skb->priority = *classid;
    }
    return bpf_redirect(ifb, 0);
}

// Ingress (SAFE variant): classify by the recorded flow and set skb->priority,
// then return TC_ACT_UNSPEC so a following `tc` filter (matchall + mirred
// egress redirect dev ifb) performs the actual, properly-reinjecting redirect.
// This program never redirects or drops, so it cannot blackhole traffic.
SEC("classifier")
int curb_ingress_setprio(struct __sk_buff *skb)
{
    struct pkt5 p;
    if (parse_pkt(skb, &p) == 0) {
        struct flow_key k = {
            .saddr = p.saddr,
            .daddr = p.daddr,
            .sport = p.sport,
            .dport = p.dport,
            .proto = p.proto,
        };
        __u32 *classid = bpf_map_lookup_elem(&flow_classid, &k);
        if (classid)
            skb->priority = *classid;
    }
    return TC_ACT_UNSPEC; // continue to the mirred redirect filter
}

char _license[] SEC("license") = "GPL";
