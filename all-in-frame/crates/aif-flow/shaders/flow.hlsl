// All In Frame - Pyramidal Hierarchical Optical Flow, HUD Saliency & Disocclusion Detection
// Target: cs_5_0

cbuffer FlowConstants : register(b0)
{
    float2 g_Resolution;        // Screen width, height
    float  g_MotionThreshold;   // Threshold below which motion is treated as static HUD
    float  g_HudConfidence;     // HUD isolation aggressiveness
};

// Convert RGB to Luminance
float Luminance(float3 color)
{
    return dot(color, float3(0.299f, 0.587f, 0.114f));
}

// =========================================================================
// Pass 1: Coarse Optical Flow (Half-Resolution, Level 1)
// Dispatched at (width/2, height/2) to track large motions up to 64 pixels
// =========================================================================

Texture2D<float4>   g_PrevFrameCoarse : register(t0);
Texture2D<float4>   g_CurrFrameCoarse : register(t1);
RWTexture2D<float2> g_CoarseOutput    : register(u0);

[numthreads(16, 16, 1)]
void CSFlowCoarseMain(uint3 dispatchThreadID : SV_DispatchThreadID)
{
    uint2 coord = dispatchThreadID.xy;
    uint2 halfRes = uint2((uint)g_Resolution.x / 2, (uint)g_Resolution.y / 2);
    if (coord.x >= halfRes.x || coord.y >= halfRes.y)
    {
        return;
    }

    int2 c = int2(coord) * 2;

    float sumIx2  = 0.0f;
    float sumIy2  = 0.0f;
    float sumIxIy = 0.0f;
    float sumIxIt = 0.0f;
    float sumIyIt = 0.0f;

    [unroll]
    for (int dy = -3; dy <= 3; dy += 2)
    {
        [unroll]
        for (int dx = -3; dx <= 3; dx += 2)
        {
            int2 p = clamp(c + int2(dx, dy), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pxPlus  = clamp(p + int2(2, 0), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pxMinus = clamp(p - int2(2, 0), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pyPlus  = clamp(p + int2(0, 2), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pyMinus = clamp(p - int2(0, 2), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));

            float currLum = Luminance(g_CurrFrameCoarse.Load(int3(p, 0)).rgb);
            float prevLum = Luminance(g_PrevFrameCoarse.Load(int3(p, 0)).rgb);

            float ix = (Luminance(g_CurrFrameCoarse.Load(int3(pxPlus, 0)).rgb) - Luminance(g_CurrFrameCoarse.Load(int3(pxMinus, 0)).rgb)) * 0.25f;
            float iy = (Luminance(g_CurrFrameCoarse.Load(int3(pyPlus, 0)).rgb) - Luminance(g_CurrFrameCoarse.Load(int3(pyMinus, 0)).rgb)) * 0.25f;
            float it = currLum - prevLum;

            sumIx2  += ix * ix;
            sumIy2  += iy * iy;
            sumIxIy += ix * iy;
            sumIxIt += ix * it;
            sumIyIt += iy * it;
        }
    }

    float det = sumIx2 * sumIy2 - sumIxIy * sumIxIy;
    float2 velocity = float2(0.0f, 0.0f);

    if (abs(det) > 1e-4f)
    {
        float invDet = 1.0f / det;
        velocity.x = -(sumIy2 * sumIxIt - sumIxIy * sumIyIt) * invDet;
        velocity.y = -(sumIx2 * sumIyIt - sumIxIy * sumIxIt) * invDet;
    }

    // Coarse scale limit (up to 32 pixels at half-res = 64 pixels at full-res)
    velocity = clamp(velocity, float2(-32.0f, -32.0f), float2(32.0f, 32.0f));
    g_CoarseOutput[coord] = velocity;
}

// =========================================================================
// Pass 2: Fine Optical Flow (Full-Resolution, Level 0) + Disocclusion Map
// =========================================================================

Texture2D<float4>   g_PrevFrame     : register(t0);
Texture2D<float4>   g_CurrFrame     : register(t1);
Texture2D<float2>   g_CoarseInput   : register(t2);

RWTexture2D<float2> g_MotionVectors : register(u0);
RWTexture2D<float2> g_Masks         : register(u1); // x: isHud, y: isOcc

[numthreads(16, 16, 1)]
void CSFlowMain(uint3 dispatchThreadID : SV_DispatchThreadID)
{
    uint2 coord = dispatchThreadID.xy;
    if (coord.x >= (uint)g_Resolution.x || coord.y >= (uint)g_Resolution.y)
    {
        return;
    }

    int2 c = int2(coord);

    // Initial coarse guess upscaled to full resolution
    float2 coarseV = g_CoarseInput.Load(int3(coord / 2, 0)) * 2.0f;
    int2 offsetV = int2(round(coarseV));

    float sumIx2  = 0.0f;
    float sumIy2  = 0.0f;
    float sumIxIy = 0.0f;
    float sumIxIt = 0.0f;
    float sumIyIt = 0.0f;

    [unroll]
    for (int dy = -2; dy <= 2; ++dy)
    {
        [unroll]
        for (int dx = -2; dx <= 2; ++dx)
        {
            int2 pCurr = clamp(c + int2(dx, dy), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pPrev = clamp(c + int2(dx, dy) - offsetV, int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));

            int2 pxPlus  = clamp(pCurr + int2(1, 0), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pxMinus = clamp(pCurr - int2(1, 0), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pyPlus  = clamp(pCurr + int2(0, 1), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
            int2 pyMinus = clamp(pCurr - int2(0, 1), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));

            float currLum = Luminance(g_CurrFrame.Load(int3(pCurr, 0)).rgb);
            float prevLum = Luminance(g_PrevFrame.Load(int3(pPrev, 0)).rgb);

            float ix = (Luminance(g_CurrFrame.Load(int3(pxPlus, 0)).rgb) - Luminance(g_CurrFrame.Load(int3(pxMinus, 0)).rgb)) * 0.5f;
            float iy = (Luminance(g_CurrFrame.Load(int3(pyPlus, 0)).rgb) - Luminance(g_CurrFrame.Load(int3(pyMinus, 0)).rgb)) * 0.5f;
            float it = currLum - prevLum;

            sumIx2  += ix * ix;
            sumIy2  += iy * iy;
            sumIxIy += ix * iy;
            sumIxIt += ix * it;
            sumIyIt += iy * it;
        }
    }

    float det = sumIx2 * sumIy2 - sumIxIy * sumIxIy;
    float2 deltaV = float2(0.0f, 0.0f);

    if (abs(det) > 1e-4f)
    {
        float invDet = 1.0f / det;
        deltaV.x = -(sumIy2 * sumIxIt - sumIxIy * sumIyIt) * invDet;
        deltaV.y = -(sumIx2 * sumIyIt - sumIxIy * sumIxIt) * invDet;
    }

    deltaV = clamp(deltaV, float2(-8.0f, -8.0f), float2(8.0f, 8.0f));
    float2 totalVelocity = coarseV + deltaV;
    totalVelocity = clamp(totalVelocity, float2(-64.0f, -64.0f), float2(64.0f, 64.0f));

    // 1. Static HUD Saliency
    float speed = length(totalVelocity);
    float isHud = (speed < g_MotionThreshold) ? 1.0f : 0.0f;

    // 2. Disocclusion Inpainting Metric (Forward-Backward consistency)
    // Check if the pixel warped back to PrevFrame matches the current pixel
    int2 pWarped = clamp(c - int2(round(totalVelocity)), int2(0, 0), int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1));
    float3 currCol = g_CurrFrame.Load(int3(c, 0)).rgb;
    float3 warpedPrevCol = g_PrevFrame.Load(int3(pWarped, 0)).rgb;
    float colorDiff = length(currCol - warpedPrevCol);

    // If fast motion produces severe color mismatch, this is an occlusion boundary/tear
    float isOcc = (speed > 2.0f) ? saturate((colorDiff - 0.18f) * 3.0f) : 0.0f;

    g_MotionVectors[coord] = totalVelocity;
    g_Masks[coord] = float2(isHud, isOcc);
}
