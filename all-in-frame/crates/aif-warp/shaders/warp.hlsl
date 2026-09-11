// All In Frame - Asynchronous Mouse Warp & Predictive Reprojection Compute Shader
// Target: cs_5_0

cbuffer WarpConstants : register(b0)
{
    float3x3 g_RotationMatrix;        // 3x3 camera rotation delta (yaw, pitch)
    float2   g_ScreenSize;            // Screen dimensions (width, height)
    float    g_TanHalfFovX;           // tan(FovX * 0.5)
    float    g_TanHalfFovY;           // tan(FovY * 0.5)
    float    g_NearPlane;             // Near clipping plane distance
    float    g_FarPlane;              // Far clipping plane distance
    uint     g_IsReverseZ;            // 1 if reverse-Z depth buffer, 0 otherwise
    uint     g_HudMaskEnabled;        // 1 to preserve static 2D UI elements
    float    g_HudDepthThreshold;     // Depth threshold for identifying 2D HUD
    float    g_InpaintingStrength;    // Bilateral inpainting strength [0.0..1.0]
    uint     g_DebugDepth;            // 1 to render depth buffer as color heatmap
    uint     g_FpsMultiplier;         // Frame generation multiplier (1, 2, 3)
};

Texture2D<float4>   g_SourceColor  : register(t0);
Texture2D<float>    g_SourceDepth  : register(t1);
RWTexture2D<float4> g_OutputWarped : register(u0);

SamplerState g_LinearClampSampler : register(s0);

// Linearizes non-linear hardware depth into camera-space Z (meters)
float LinearizeDepth(float zRaw)
{
    zRaw = saturate(zRaw);
    if (g_IsReverseZ != 0)
    {
        if (zRaw < 1e-6) return g_FarPlane;
        return g_NearPlane / zRaw;
    }
    else
    {
        float denom = g_FarPlane - zRaw * (g_FarPlane - g_NearPlane);
        if (abs(denom) < 1e-6) return g_FarPlane;
        return (g_NearPlane * g_FarPlane) / denom;
    }
}

[numthreads(16, 16, 1)]
void CSMain(uint3 dispatchThreadID : SV_DispatchThreadID)
{
    uint2 pixelCoord = dispatchThreadID.xy;
    if (pixelCoord.x >= (uint)g_ScreenSize.x || pixelCoord.y >= (uint)g_ScreenSize.y)
    {
        return;
    }

    float2 uv = (float2(pixelCoord) + 0.5f) / g_ScreenSize;
    float rawDepth = g_SourceDepth.Load(int3(pixelCoord, 0));

    // 1. HUD / UI Isolation Pass:
    // UI elements (crosshair, health bars, radar) are typically rendered directly
    // to the backbuffer without depth write or with depth pinned at 0.0 or 1.0.
    // If HUD mask is enabled, keep original pixel without 3D warping.
    if (g_HudMaskEnabled != 0)
    {
        // UI elements (crosshair, health bars, radar) are drawn at the camera near-plane:
        // Standard Z: Near plane is 0.0 -> rawDepth <= threshold
        // Reverse-Z:  Near plane is 1.0 -> rawDepth >= (1.0 - threshold)
        bool isHud = (g_IsReverseZ == 0) ? (rawDepth <= g_HudDepthThreshold) : (rawDepth >= (1.0f - g_HudDepthThreshold));
        if (isHud)
        {
            g_OutputWarped[pixelCoord] = g_SourceColor.Load(int3(pixelCoord, 0));
            return;
        }
    }

    float zLinear = LinearizeDepth(rawDepth);

    // Visual Depth Debug Mode (Heatmap):
    if (g_DebugDepth != 0)
    {
        float normZ = saturate(log(max(zLinear, 0.1f)) / 4.0f);
        float3 debugColor = float3(normZ, 1.0f - abs(normZ * 2.0f - 1.0f), 1.0f - normZ);
        g_OutputWarped[pixelCoord] = float4(debugColor, 1.0f);
        return;
    }

    // 2. Unproject screen UV + linear depth into 3D View Space:
    float ndcX = uv.x * 2.0f - 1.0f;
    float ndcY = 1.0f - uv.y * 2.0f;

    float3 pView;
    pView.x = ndcX * g_TanHalfFovX * zLinear;
    pView.y = ndcY * g_TanHalfFovY * zLinear;
    pView.z = zLinear;

    // 3. Apply Camera Rotation Delta:
    // P'_view = R * P_view
    float3 pViewRotated = mul(g_RotationMatrix, pView);

    // If geometry rotates behind the camera plane, fall back to source
    if (pViewRotated.z <= 1e-3f)
    {
        g_OutputWarped[pixelCoord] = g_SourceColor.Load(int3(pixelCoord, 0));
        return;
    }

    // 4. Reproject into 2D Screen UV:
    float targetNdcX = pViewRotated.x / (pViewRotated.z * g_TanHalfFovX);
    float targetNdcY = pViewRotated.y / (pViewRotated.z * g_TanHalfFovY);

    float2 targetUv;
    targetUv.x = targetNdcX * 0.5f + 0.5f;
    targetUv.y = 0.5f - targetNdcY * 0.5f;

    // 5. Bilinear sample warped color:
    if (targetUv.x >= 0.0f && targetUv.x <= 1.0f && targetUv.y >= 0.0f && targetUv.y <= 1.0f)
    {
        float4 sampledColor = g_SourceColor.SampleLevel(g_LinearClampSampler, targetUv, 0.0f);
        
        // Disocclusion handling: If depth gradient is severe, blend with neighbor
        float sampledRawDepth = g_SourceDepth.SampleLevel(g_LinearClampSampler, targetUv, 0.0f);
        float sampledZLinear = LinearizeDepth(sampledRawDepth);
        float depthDiff = abs(sampledZLinear - pViewRotated.z);

        if (depthDiff > 0.5f * pViewRotated.z && g_InpaintingStrength > 0.01f)
        {
            // Edge inpainting: soft blend towards unwarped background
            float4 fallbackColor = g_SourceColor.Load(int3(pixelCoord, 0));
            sampledColor = lerp(sampledColor, fallbackColor, g_InpaintingStrength * 0.5f);
        }

        g_OutputWarped[pixelCoord] = sampledColor;
    }
    else
    {
        // Edge boundary fill (clamp to source color at boundary)
        g_OutputWarped[pixelCoord] = g_SourceColor.Load(int3(pixelCoord, 0));
    }

    // 6. On-Screen Display (OSD): Dynamic badge in top-left corner
    // Color coded according to Frame Generation Multiplier:
    // 1x = Emerald Green
    // 2x = Electric Cyan (2x Frame Generation)
    // 3x = Neon Purple / Violet (3x Frame Generation)
    if (pixelCoord.x >= 20 && pixelCoord.x <= 220 && pixelCoord.y >= 20 && pixelCoord.y <= 52)
    {
        float4 themeColor = float4(0.0f, 0.95f, 0.55f, 1.0f); // 1x Green
        if (g_FpsMultiplier == 2)
        {
            themeColor = float4(0.0f, 0.85f, 1.0f, 1.0f);     // 2x Cyan
        }
        else if (g_FpsMultiplier >= 3)
        {
            themeColor = float4(0.9f, 0.25f, 1.0f, 1.0f);     // 3x Purple
        }

        bool isBorder = (pixelCoord.x == 20 || pixelCoord.x == 220 || pixelCoord.y == 20 || pixelCoord.y == 52);
        if (isBorder)
        {
            g_OutputWarped[pixelCoord] = themeColor;
        }
        else
        {
            float distDot = length(float2(pixelCoord) - float2(36.0f, 36.0f));
            if (distDot < 5.0f)
            {
                g_OutputWarped[pixelCoord] = themeColor;
            }
            else
            {
                float4 bg = float4(0.02f, 0.05f, 0.08f, 1.0f);
                g_OutputWarped[pixelCoord] = lerp(g_OutputWarped[pixelCoord], bg, 0.85f);
            }
        }
    }
}
