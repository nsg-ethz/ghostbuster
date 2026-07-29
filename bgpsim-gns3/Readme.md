# Requirements

Make sure to have `gns3-server` installed, and make sure that `gns3server` is running locally, on port 3080. Also, make sure that the Docker Daemon is running.

Finally, install the frr image as follows (the correct templates are created if they are missing!):

```sh
docker build -t frr docker-frr
docker pull gns3/ipterm
```


