from setuptools import setup

PACKAGE = "vig_bridge"

setup(
    name=PACKAGE,
    version="0.1.0",
    packages=[PACKAGE],
    data_files=[
        ("share/ament_index/resource_index/packages", ["resource/" + PACKAGE]),
        ("share/" + PACKAGE, ["package.xml"]),
    ],
    install_requires=["setuptools", "numpy", "grpcio", "tritonclient[grpc]"],
    zip_safe=True,
    maintainer="Vigilant e.K.",
    maintainer_email="info@vigilant-crs.de",
    description="ROS 2 camera topics to the Vigilant Inference Governor over OIP",
    license="BUSL-1.1",
    tests_require=["pytest"],
    entry_points={"console_scripts": ["vig_bridge = vig_bridge.node:main"]},
)
