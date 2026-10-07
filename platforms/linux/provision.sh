#!/bin/sh
set -eu

interface=${1:-barracuda0}
owner=${SUDO_USER:-${USER:?USER is not set}}
outbound=$(ip route show default | awk 'NR == 1 { print $5 }')

if ! ip link show "$interface" >/dev/null 2>&1; then
    ip tuntap add dev "$interface" mode tun user "$owner"
fi
ip address replace 10.42.0.1 peer 10.42.0.2 dev "$interface"
ip link set dev "$interface" mtu 1500 up
sysctl -w net.ipv4.ip_forward=1 >/dev/null

iptables -t nat -C POSTROUTING -s 10.42.0.0/30 -o "$outbound" -j MASQUERADE 2>/dev/null \
    || iptables -t nat -A POSTROUTING -s 10.42.0.0/30 -o "$outbound" -j MASQUERADE
# The System's stack ignores ICMP "fragmentation needed", so a full-size
# segment is silently lost when the outbound link has a smaller MTU than the
# TUN. Clamp the MSS of forwarded connections to the path MTU instead.
iptables -t mangle -C FORWARD -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu 2>/dev/null \
    || iptables -t mangle -A FORWARD -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu
iptables -C FORWARD -i "$interface" -o "$outbound" -j ACCEPT 2>/dev/null \
    || iptables -A FORWARD -i "$interface" -o "$outbound" -j ACCEPT
iptables -C FORWARD -i "$outbound" -o "$interface" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null \
    || iptables -A FORWARD -i "$outbound" -o "$interface" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT

echo "Linux TUN $interface is ready for user $owner via $outbound"
