// NV-09: was ein direkter TensorRT-Pfad gegenueber Triton bringt.
//
// Kein Executor, ein Durchstich: Engine laden, feste Shapes, ein Auftrag je
// Aufruf, CUDA-Endevent statt Zeitmessung auf der Hostseite. Genau die
// Bausteine, die der Liefergegenstand nennt — und die Frage, ob sich der Weg
// lohnt.
#include <NvInfer.h>
#include <cuda_runtime.h>
#include <chrono>
#include <cstdio>
#include <fstream>
#include <vector>
#include <algorithm>

class Silent : public nvinfer1::ILogger {
    void log(Severity s, const char *msg) noexcept override {
        if (s <= Severity::kERROR) printf("[TRT] %s\n", msg);
    }
} logger;

int main(int argc, char **argv) {
    setvbuf(stdout, nullptr, _IONBF, 0);
    if (argc < 2) { printf("Aufruf: probe <engine> [laeufe]\n"); return 2; }
    const int runs = argc > 2 ? atoi(argv[2]) : 300;

    std::ifstream f(argv[1], std::ios::binary);
    if (!f) { printf("Engine nicht lesbar: %s\n", argv[1]); return 1; }
    std::vector<char> blob((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    printf("Engine: %s, %.1f MB\n", argv[1], blob.size() / 1048576.0);

    auto *runtime = nvinfer1::createInferRuntime(logger);
    auto *engine = runtime->deserializeCudaEngine(blob.data(), blob.size());
    if (!engine) { printf("deserializeCudaEngine fehlgeschlagen\n"); return 1; }
    auto *ctx = engine->createExecutionContext();

    // Feste Shapes: alle Tensoren bekommen einen Puffer, Groesse aus der
    // Engine. Kein dynamisches Batching, kein Wachstum zur Laufzeit.
    cudaStream_t stream; cudaStreamCreate(&stream);
    std::vector<void *> buffers;
    for (int i = 0; i < engine->getNbIOTensors(); ++i) {
        const char *name = engine->getIOTensorName(i);
        auto dims = ctx->getTensorShape(name);
        size_t elems = 1;
        for (int d = 0; d < dims.nbDims; ++d) elems *= (size_t)(dims.d[d] > 0 ? dims.d[d] : 1);
        void *p = nullptr;
        cudaMalloc(&p, elems * sizeof(float));
        cudaMemset(p, 0, elems * sizeof(float));
        buffers.push_back(p);
        ctx->setTensorAddress(name, p);
        printf("  %-8s %s, %zu Elemente\n",
               engine->getTensorIOMode(name) == nvinfer1::TensorIOMode::kINPUT ? "Eingang" : "Ausgang",
               name, elems);
    }

    // CUDA-Endevent statt Hostuhr: es sagt, wann die **Karte** fertig ist.
    cudaEvent_t done; cudaEventCreateWithFlags(&done, cudaEventBlockingSync);

    for (int i = 0; i < 30; ++i) { ctx->enqueueV3(stream); cudaStreamSynchronize(stream); }

    std::vector<double> us;
    us.reserve(runs);
    for (int i = 0; i < runs; ++i) {
        auto t = std::chrono::steady_clock::now();
        if (!ctx->enqueueV3(stream)) { printf("enqueueV3 fehlgeschlagen\n"); return 1; }
        cudaEventRecord(done, stream);
        cudaEventSynchronize(done);
        us.push_back(std::chrono::duration<double, std::micro>(
            std::chrono::steady_clock::now() - t).count());
    }
    std::sort(us.begin(), us.end());
    auto pick = [&](double q) { return us[(size_t)(us.size() * q) < us.size() ? (size_t)(us.size() * q) : us.size() - 1]; };
    printf("Direkt, %d Laeufe: p50 %.0f us | p95 %.0f us | p99 %.0f us\n",
           runs, pick(0.50), pick(0.95), pick(0.99));

    cudaEventDestroy(done);
    for (void *p : buffers) cudaFree(p);
    delete ctx; delete engine; delete runtime;
    return 0;
}
