# Testumgebung fuer die ROS-2-Bruecke: ROS 2 Jazzy plus die Python-Pakete,
# die ein ROS-Rechner fuer den OIP-Transport braucht.
#
# Die Quellen stecken nicht im Image; sie werden beim Lauf eingehaengt:
#
#   docker build -t vig-ros2-test integrations/ros2/docker
#   docker run --rm -v "$PWD/integrations/ros2/vig_bridge:/ws/vig_bridge" vig-ros2-test
FROM ros:jazzy-ros-base

RUN apt-get update \
 && apt-get install -y --no-install-recommends python3-pip python3-pytest \
 && rm -rf /var/lib/apt/lists/*

# Ubuntu 24.04 schuetzt das System-Python (PEP 668). Das Image ist eine
# Wegwerf-Testumgebung, deshalb ist die Freigabe hier vertretbar.
RUN pip3 install --no-cache-dir --break-system-packages "tritonclient[grpc]>=2.50"

WORKDIR /ws/vig_bridge
# Kein Bytecode in den eingehaengten Quellbaum: der Container laeuft als
# root, und root-eigene __pycache__ im Arbeitsverzeichnis stoeren danach.
ENV PYTHONPATH=/ws/vig_bridge PYTHONDONTWRITEBYTECODE=1
CMD ["python3", "-m", "pytest", "-q", "test"]
