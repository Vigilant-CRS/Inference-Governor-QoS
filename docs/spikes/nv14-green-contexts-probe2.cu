// NV-14, zweiter Teil: begrenzt die Partition tatsaechlich, und schuetzt sie
// gegen einen Speicherbandbreitengegner?
#include <cstdio>
#include <chrono>
#include <vector>
#include <cuda.h>
#include <cuda_runtime.h>

#define CHK(x) do { CUresult r = (x); if (r != CUDA_SUCCESS) { \
    const char *n = nullptr; cuGetErrorName(r, &n); \
    printf("  FEHLER %s -> %s\n", #x, n ? n : "?"); return 1; } } while (0)

// Rechenlastig: viele Bloecke, jeder rechnet lange. Skaliert mit SMs.
__global__ void compute(long long n, float *s) {
    float a = 0; for (long long i = 0; i < n; ++i) a += __sinf((float)(i + blockIdx.x));
    if (threadIdx.x == 1024) *s = a;
}
// Bandbreitenlastig: liest und schreibt viel, rechnet fast nichts.
__global__ void bandwidth(float *dst, const float *src, size_t n) {
    size_t i = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
    size_t stride = (size_t)gridDim.x * blockDim.x;
    for (; i < n; i += stride) dst[i] = src[i] * 1.000001f;
}

static double ms(std::chrono::steady_clock::time_point t) {
    return std::chrono::duration<double, std::milli>(
        std::chrono::steady_clock::now() - t).count();
}

int main() {
    setvbuf(stdout, nullptr, _IONBF, 0);
    CHK(cuInit(0));
    CUdevice dev; CHK(cuDeviceGet(&dev, 0));
    int sms = 0;
    CHK(cuDeviceGetAttribute(&sms, CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT, dev));
    CUcontext primary; CHK(cuDevicePrimaryCtxRetain(&primary, dev));
    CHK(cuCtxSetCurrent(primary));

    CUdevResource whole;
    CHK(cuDeviceGetDevResource(dev, &whole, CU_DEV_RESOURCE_TYPE_SM));

    float *sink, *big_a, *big_b;
    const size_t elems = 64u << 20; // 256 MB je Puffer
    cudaMalloc(&sink, sizeof(float));
    cudaMalloc(&big_a, elems * sizeof(float));
    cudaMalloc(&big_b, elems * sizeof(float));
    cudaMemset(big_a, 0, elems * sizeof(float));

    const int blocks = sms * 4;
    const long long work = 60000;

    // 1) Volle Karte.
    cudaStream_t full; cudaStreamCreate(&full);
    for (int w = 0; w < 2; ++w) { compute<<<blocks, 256, 0, full>>>(work, sink); cudaStreamSynchronize(full); }
    auto t = std::chrono::steady_clock::now();
    compute<<<blocks, 256, 0, full>>>(work, sink);
    cudaStreamSynchronize(full);
    const double whole_ms = ms(t);
    printf("Rechenkernel, ganze Karte (%d SMs, %d Bloecke): %.1f ms\n", sms, blocks, whole_ms);

    // 2) Eine Partition mit einem Viertel der SMs.
    unsigned want = (unsigned)(sms / 4) & ~1u;
    std::vector<CUdevResource> parts(4);
    unsigned n = 2; CUdevResource rest;
    CHK(cuDevSmResourceSplitByCount(parts.data(), &n, &whole, &rest, 0, want));

    CUgreenCtx g[2]; CUstream st[2];
    for (int i = 0; i < 2; ++i) {
        CUdevResourceDesc d; CHK(cuDevResourceGenerateDesc(&d, &parts[i], 1));
        CHK(cuGreenCtxCreate(&g[i], d, dev, CU_GREEN_CTX_DEFAULT_STREAM));
        CUcontext gc; CHK(cuCtxFromGreenCtx(&gc, g[i]));
        CHK(cuCtxPushCurrent(gc)); CHK(cuStreamCreate(&st[i], CU_STREAM_NON_BLOCKING));
        CUcontext p; CHK(cuCtxPopCurrent(&p));
    }
    printf("Partition: je %u SMs (von %d)\n", parts[0].sm.smCount, sms);

    compute<<<blocks, 256, 0, (cudaStream_t)st[0]>>>(work, sink);
    cuStreamSynchronize(st[0]);
    t = std::chrono::steady_clock::now();
    compute<<<blocks, 256, 0, (cudaStream_t)st[0]>>>(work, sink);
    cuStreamSynchronize(st[0]);
    const double part_ms = ms(t);
    printf("Rechenkernel, eine Partition: %.1f ms (%.2fx der ganzen Karte)\n",
           part_ms, part_ms / whole_ms);
    printf("  erwartet bei echter Begrenzung: %.2fx\n", (double)sms / parts[0].sm.smCount);

    // 3) Bandbreitengegner in der Nachbarpartition.
    t = std::chrono::steady_clock::now();
    compute<<<blocks, 256, 0, (cudaStream_t)st[0]>>>(work, sink);
    cuStreamSynchronize(st[0]);
    const double solo = ms(t);

    bandwidth<<<blocks, 256, 0, (cudaStream_t)st[1]>>>(big_b, big_a, elems);
    t = std::chrono::steady_clock::now();
    compute<<<blocks, 256, 0, (cudaStream_t)st[0]>>>(work, sink);
    cuStreamSynchronize(st[0]);
    const double under = ms(t);
    cuStreamSynchronize(st[1]);
    printf("Rechenkernel allein in seiner Partition: %.1f ms\n", solo);
    printf("Rechenkernel neben Bandbreitengegner:   %.1f ms (%.2fx)\n", under, under / solo);

    // 4) Und jetzt ein **bandbreitengebundenes** Opfer neben demselben
    // Gegner. Green Contexts teilen SMs, nicht Speicherbandbreite — genau
    // hier muss sich das zeigen.
    float *v_a, *v_b;
    cudaMalloc(&v_a, elems * sizeof(float));
    cudaMalloc(&v_b, elems * sizeof(float));
    cudaMemset(v_a, 0, elems * sizeof(float));

    bandwidth<<<blocks, 256, 0, (cudaStream_t)st[0]>>>(v_b, v_a, elems);
    cuStreamSynchronize(st[0]);
    t = std::chrono::steady_clock::now();
    bandwidth<<<blocks, 256, 0, (cudaStream_t)st[0]>>>(v_b, v_a, elems);
    cuStreamSynchronize(st[0]);
    const double bw_solo = ms(t);

    for (int i = 0; i < 4; ++i) bandwidth<<<blocks, 256, 0, (cudaStream_t)st[1]>>>(big_b, big_a, elems);
    t = std::chrono::steady_clock::now();
    bandwidth<<<blocks, 256, 0, (cudaStream_t)st[0]>>>(v_b, v_a, elems);
    cuStreamSynchronize(st[0]);
    const double bw_under = ms(t);
    cuStreamSynchronize(st[1]);
    printf("Bandbreitenkernel allein in seiner Partition: %.1f ms\n", bw_solo);
    printf("Bandbreitenkernel neben Bandbreitengegner:    %.1f ms (%.2fx)\n",
           bw_under, bw_under / bw_solo);
    printf("  Green Contexts teilen SMs, nicht Bandbreite: %s\n",
           bw_under > bw_solo * 1.2 ? "der Gegner kommt durch" : "kein Effekt gemessen");

    cudaFree(v_a); cudaFree(v_b);
    cudaFree(sink); cudaFree(big_a); cudaFree(big_b);
    return 0;
}
