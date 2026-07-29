while true; do /usr/lib/frr/docker-start; done &

sleep 2

while true; do /usr/bin/vtysh; /bin/sh; done
