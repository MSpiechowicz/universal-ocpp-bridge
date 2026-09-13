# Build context is assembled by scripts/test-external-csms.sh, never the repository.
FROM python:3.14.7-slim-trixie@sha256:cad9a2c871761c413caa6fdd6441c783451e740a48aaeba60ae62a8b53525ef6
ENV PYTHONDONTWRITEBYTECODE=1 PIP_DISABLE_PIP_VERSION_CHECK=1
COPY requirements.txt /opt/external-csms/requirements.txt
RUN pip install --no-cache-dir --require-hashes -r /opt/external-csms/requirements.txt
COPY uob-sim /usr/local/bin/uob-sim
COPY peer.py run.py test_evidence.py /opt/external-csms/
COPY scenarios /opt/external-csms/scenarios
WORKDIR /opt/external-csms
USER 65534:65534
ENTRYPOINT ["python", "run.py", "--simulator", "/usr/local/bin/uob-sim"]
