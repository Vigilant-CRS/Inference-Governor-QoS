// NV-14: Capability-Probe fuer Green Contexts auf dieser Karte.
//
// Beantwortet die Abnahmefragen der Reihe nach: laesst sich der SM-Satz
// aufteilen, wie fein, was kommt tatsaechlich heraus, und laufen zwei
// disjunkte Partitionen wirklich nebeneinander.
#include <cstdio>
#include <chrono>
#include <vector>
#include <cuda.h>
#include <cuda_runtime.h>

#define CHK(x) do { CUresult r = (x); if (r != CUDA_SUCCESS) { \
    const char *n = nullptr; cuGetErrorName(r, &n); \
    printf("  FEHLER %s -> %s\n", #x, n ? n : "?"); return 1; } } while (0)

__global__ void spin(long long n, float *s) {
    float a = 0; for (long long i = 0; i < n; ++i) a += __sinf((float)i);
    if (threadIdx.x == 1024) *s = a;
}

static double ms(std::chrono::steady_clock::time_point t) {
    return std::chrono::duration<double, std::milli>(
        std::chrono::steady_clock::now() - t).count();
}

int main() {
    setvbuf(stdout, nullptr, _IONBF, 0);
    CHK(cuInit(0));
    CUdevice dev; CHK(cuDeviceGet(&dev, 0));
    char name[128]; CHK(cuDeviceGetName(name, sizeof(name), dev));
    int major = 0, minor = 0, sms = 0;
    CHK(cuDeviceGetAttribute(&major, CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR, dev));
    CHK(cuDeviceGetAttribute(&minor, CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR, dev));
    CHK(cuDeviceGetAttribute(&sms, CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT, dev));
    printf("Geraet: %s, sm%d%d, %d SMs\n", name, major, minor, sms);

    CUcontext primary; CHK(cuDevicePrimaryCtxRetain(&primary, dev));
    CHK(cuCtxSetCurrent(primary));

    CUdevResource whole;
    CHK(cuDeviceGetDevResource(dev, &whole, CU_DEV_RESOURCE_TYPE_SM));
    printf("Ganzer SM-Satz: %u SMs\n", whole.sm.smCount);

    // Aufteilen. Der Header nennt fuer 8.x mindestens 4 SMs, Vielfaches von 2.
    for (unsigned want : {2u, 4u, 8u, 16u}) {
        unsigned groups = whole.sm.smCount / want;
        std::vector<CUdevResource> parts(groups);
        CUdevResource rest;
        unsigned n = groups;
        CUresult r = cuDevSmResourceSplitByCount(parts.data(), &n, &whole, &rest, 0, want);
        if (r != CUDA_SUCCESS) {
            const char *e = nullptr; cuGetErrorName(r, &e);
            printf("  minCount %2u: %s\n", want, e ? e : "?");
            continue;
        }
        printf("  minCount %2u: %u Partitionen, je %u SMs (Rest %u)\n",
               want, n, n ? parts[0].sm.smCount : 0, rest.sm.smCount);
    }

    // Zwei disjunkte Partitionen zu je einem Viertel der Karte.
    unsigned want = (unsigned)(sms / 4) & ~1u;
    if (want < 4) want = 4;
    std::vector<CUdevResource> parts(4);
    unsigned n = 2;
    CUdevResource rest;
    CHK(cuDevSmResourceSplitByCount(parts.data(), &n, &whole, &rest, 0, want));
    printf("Zwei Partitionen: je %u SMs, Rest %u SMs\n", parts[0].sm.smCount, rest.sm.smCount);

    CUgreenCtx g[2]; CUstream st[2];
    for (int i = 0; i < 2; ++i) {
        CUdevResourceDesc desc;
        CHK(cuDevResourceGenerateDesc(&desc, &parts[i], 1));
        CHK(cuGreenCtxCreate(&g[i], desc, dev, CU_GREEN_CTX_DEFAULT_STREAM));
        // CUDA 12.4 kennt `cuGreenCtxStreamCreate` noch nicht. Der Weg ist
        // der primaere Kontext des Green Context: `cuCtxFromGreenCtx` gibt
        // ihn, und ein Stream darin gehoert zur Partition.
        CUcontext gc;
        CHK(cuCtxFromGreenCtx(&gc, g[i]));
        CHK(cuCtxPushCurrent(gc));
        CHK(cuStreamCreate(&st[i], CU_STREAM_NON_BLOCKING));
        CUcontext popped;
        CHK(cuCtxPopCurrent(&popped));
        CUdevResource got;
        CHK(cuGreenCtxGetDevResource(g[i], &got, CU_DEV_RESOURCE_TYPE_SM));
        printf("Green-Context %d: angefordert %u, bekommen %u SMs\n", i, want, got.sm.smCount);
    }

    float *sink; cudaMalloc(&sink, sizeof(float));
    const long long work = 400000;

    auto t = std::chrono::steady_clock::now();
    spin<<<1, 256, 0, (cudaStream_t)st[0]>>>(work, sink);
    cuStreamSynchronize(st[0]);
    const double alone = ms(t);

    t = std::chrono::steady_clock::now();
    spin<<<1, 256, 0, (cudaStream_t)st[0]>>>(work, sink);
    spin<<<1, 256, 0, (cudaStream_t)st[1]>>>(work, sink);
    cuStreamSynchronize(st[0]); cuStreamSynchronize(st[1]);
    const double both = ms(t);

    printf("Ein Kernel in einer Partition: %.1f ms\n", alone);
    printf("Zwei Kernel in disjunkten Partitionen: %.1f ms\n", both);
    printf("Nebenlaeufig: %s (%.2fx)\n",
           both < alone * 1.5 ? "ja" : "NEIN, serialisiert", both / alone);
    return 0;
}
