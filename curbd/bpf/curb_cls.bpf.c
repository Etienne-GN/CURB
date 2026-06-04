// CURB eBPF traffic classifier.
//
// Attached to a clsact hook, this program classifies each packet to a per-app
// HTB class by writing skb->priority to the class handle. The root HTB qdisc
// then enqueues the packet into that class, giving smooth *shaping* (queuing)
// rather than the drop-based policing of the nftables path.
//
// Egress: the skb already carries the sending socket, so its cgroup id is read
// directly with bpf_skb_cgroup_id().
//
// Userspace (curbd, via Aya) populates CGROUP_CLASSID with cgroup_id -> classid
// for each tracked app, and creates the matching HTB classes.
//
// Build: clang -O2 -g -target bpf -c curb_cls.bpf.c -o curb_cls.bpf.o

#include <linux/bpf.h>
#include <linux/pkt_cls.h>
#include <bpf/bpf_helpers.h>

// cgroup v2 id -> tc class handle (e.g. 0x00010010 for class 1:16), used as the
// value written to skb->priority so HTB selects that class.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, __u64);
    __type(value, __u32);
} cgroup_classid SEC(".maps");

// Egress classifier: map the sending socket's cgroup to its HTB class.
SEC("classifier")
int curb_egress(struct __sk_buff *skb)
{
    __u64 cgid = bpf_skb_cgroup_id(skb);
    if (cgid) {
        __u32 *classid = bpf_map_lookup_elem(&cgroup_classid, &cgid);
        if (classid)
            skb->priority = *classid;
    }
    return TC_ACT_OK;
}

char _license[] SEC("license") = "GPL";
