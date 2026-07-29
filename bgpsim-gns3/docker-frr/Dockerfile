FROM quay.io/frrouting/frr:10.2.1

# copy the vtysh configuration
COPY vtysh.conf /etc/frr/vtysh.conf
COPY startup.sh /startup.sh
# enable all daemons
RUN sed -i 's/^\([a-z]*\)d=no/\1d=yes/g' /etc/frr/daemons
RUN chmod +x /startup.sh
