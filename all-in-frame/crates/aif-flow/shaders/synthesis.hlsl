// All In Frame - Frame Synthesizer, Disocclusion Inpainting, CAS & Performance HUD
// Target: cs_5_0

cbuffer SynthConstants : register(b0)
{
    float2 g_Resolution;        // Screen width, height
    float  g_Alpha;             // Interpolation time fraction [0.0..1.0]
    uint   g_FpsMultiplier;     // Frame Gen Multiplier (1, 2, 3)
    uint   g_DebugFlow;         // 1 = visualize motion vectors as HSV colors
    float  g_Sharpening;        // FidelityFX CAS strength [0.0..1.0]
    float  g_ElapsedSeconds;    // Time since generation start for smooth OSD banner fadeout
    uint   g_ShowPerfHud;       // 1 = Show in-game Frametime graph and live FPS
    float  g_GameFps;           // Measured game capture FPS
    float  g_OutputFps;         // Measured monitor output FPS
    float  g_FrametimeMs;       // Frame time in ms
    float  g_Pad;
};

Texture2D<float4>   g_PrevFrame         : register(t0);
Texture2D<float4>   g_CurrFrame         : register(t1);
Texture2D<float2>   g_MotionVectors     : register(t2);
Texture2D<float2>   g_Masks             : register(t3); // x: isHud, y: isOcc
RWTexture2D<float4> g_SynthesizedFrame  : register(u0);

SamplerState g_LinearClamp : register(s0);

// Visual motion vector color wheel (for debug mode)
float3 MotionToColor(float2 v)
{
    float angle = atan2(v.y, v.x);
    float mag = length(v) / 24.0f;
    float3 col = float3(
        0.5f + 0.5f * cos(angle),
        0.5f + 0.5f * cos(angle + 2.094f),
        0.5f + 0.5f * cos(angle + 4.188f)
    );
    return lerp(float3(0.05f, 0.05f, 0.05f), col, saturate(mag));
}

// =========================================================================
// AMD FidelityFX Contrast Adaptive Sharpening (CAS) Filter
// =========================================================================
float3 ApplyCas(int2 coord, float3 centerRgb, float sharpness)
{
    if (sharpness <= 0.01f)
    {
        return centerRgb;
    }

    int2 maxCoord = int2((int)g_Resolution.x - 1, (int)g_Resolution.y - 1);
    float3 a = g_CurrFrame.Load(int3(clamp(coord + int2( 0, -1), int2(0, 0), maxCoord), 0)).rgb;
    float3 b = g_CurrFrame.Load(int3(clamp(coord + int2(-1,  0), int2(0, 0), maxCoord), 0)).rgb;
    float3 d = g_CurrFrame.Load(int3(clamp(coord + int2( 1,  0), int2(0, 0), maxCoord), 0)).rgb;
    float3 e = g_CurrFrame.Load(int3(clamp(coord + int2( 0,  1), int2(0, 0), maxCoord), 0)).rgb;

    float3 minRgb = min(min(min(a, b), min(d, e)), centerRgb);
    float3 maxRgb = max(max(max(a, b), max(d, e)), centerRgb);

    float3 ampRgb = saturate(min(minRgb, 1.0f - maxRgb) / max(maxRgb, 1e-4f));
    float3 wRgb = -sqrt(ampRgb) * (sharpness * 0.18f);

    float3 sharpened = (a + b + d + e) * wRgb + centerRgb * (1.0f - 4.0f * wRgb);
    return saturate(sharpened);
}

[numthreads(16, 16, 1)]
void CSSynthMain(uint3 dispatchThreadID : SV_DispatchThreadID)
{
    uint2 coord = dispatchThreadID.xy;
    if (coord.x >= (uint)g_Resolution.x || coord.y >= (uint)g_Resolution.y)
    {
        return;
    }

    float2 uv = (float2(coord) + 0.5f) / g_Resolution;
    float2 v = g_MotionVectors.Load(int3(coord, 0));
    float2 masks = g_Masks.Load(int3(coord, 0));
    float isHud = masks.x;
    float isOcc = masks.y;

    // 1. Optical Flow Debug Mode:
    if (g_DebugFlow != 0)
    {
        g_SynthesizedFrame[coord] = float4(MotionToColor(v), 1.0f);
        return;
    }

    // 2. Static HUD Preservation:
    if (isHud > 0.5f)
    {
        float4 hudCol = g_CurrFrame.Load(int3(coord, 0));
        hudCol.a = 1.0f;
        g_SynthesizedFrame[coord] = hudCol;
        return;
    }

    // 3. Bidirectional Warping with Disocclusion Inpainting:
    float2 vUv = v / g_Resolution;
    float2 uvPrev = uv - g_Alpha * vUv;
    float2 uvCurr = uv + (1.0f - g_Alpha) * vUv;

    float4 colorPrev = g_PrevFrame.SampleLevel(g_LinearClamp, saturate(uvPrev), 0.0f);
    float4 colorCurr = g_CurrFrame.SampleLevel(g_LinearClamp, saturate(uvCurr), 0.0f);

    // Disocclusion Inpainting: If a silhouette boundary was uncovered,
    // smoothly favor the current pristine frame to prevent ghosting / double contours!
    float effectiveAlpha = lerp(g_Alpha, 1.0f, isOcc);
    float4 blended = lerp(colorPrev, colorCurr, effectiveAlpha);

    // 4. AMD FidelityFX CAS Sharpening Pass
    float3 sharpenedRgb = ApplyCas(int2(coord), blended.rgb, g_Sharpening);
    g_SynthesizedFrame[coord] = float4(sharpenedRgb, 1.0f);

    // 5. Activation Banner (OSD): Fades out after 3.5s
    if (g_ElapsedSeconds < 3.5f)
    {
        float fade = saturate((3.5f - g_ElapsedSeconds) / 0.8f);
        if (coord.x >= 20 && coord.x <= 240 && coord.y >= 20 && coord.y <= 54)
        {
            float4 theme = (g_FpsMultiplier == 2) ? float4(0.0f, 0.85f, 1.0f, 1.0f) : float4(0.9f, 0.25f, 1.0f, 1.0f);
            bool isBorder = (coord.x == 20 || coord.x == 240 || coord.y == 20 || coord.y == 54);
            if (isBorder)
            {
                g_SynthesizedFrame[coord] = lerp(g_SynthesizedFrame[coord], theme, fade);
            }
            else
            {
                float distDot = length(float2(coord) - float2(38.0f, 37.0f));
                if (distDot < 5.0f)
                {
                    g_SynthesizedFrame[coord] = lerp(g_SynthesizedFrame[coord], theme, fade);
                }
                else
                {
                    float4 bg = float4(0.02f, 0.05f, 0.08f, 1.0f);
                    g_SynthesizedFrame[coord] = lerp(g_SynthesizedFrame[coord], bg, 0.85f * fade);
                }
            }
        }
    }

    // 6. Cyber Performance HUD & Frametime Sparkline (Toggleable via F12)
    if (g_ShowPerfHud != 0)
    {
        int hudWidth = 260;
        int hudHeight = 74;
        int hudLeft = (int)g_Resolution.x - hudWidth - 20;
        int hudTop = 20;
        int hudRight = hudLeft + hudWidth;
        int hudBottom = hudTop + hudHeight;

        int2 c = int2(coord);
        if (c.x >= hudLeft && c.x <= hudRight && c.y >= hudTop && c.y <= hudBottom)
        {
            bool isBorder = (c.x == hudLeft || c.x == hudRight || c.y == hudTop || c.y == hudBottom);
            float4 hudTheme = float4(0.0f, 0.90f, 0.60f, 1.0f); // Neon Emerald

            if (isBorder)
            {
                g_SynthesizedFrame[coord] = hudTheme;
            }
            else
            {
                // Semi-transparent dark glass background
                float4 glassBg = float4(0.02f, 0.04f, 0.07f, 1.0f);
                float4 baseCol = lerp(g_SynthesizedFrame[coord], glassBg, 0.88f);

                // Frametime sparkline baseline
                int graphBaseY = hudBottom - 18;
                int graphHeight = (int)clamp(g_FrametimeMs * 1.5f, 2.0f, 30.0f);
                int graphTop = graphBaseY - graphHeight;

                int relX = c.x - hudLeft;
                // Draw dynamic frametime pulse line
                if (c.y == graphTop && relX > 15 && relX < hudWidth - 15)
                {
                    float4 lineColor = (g_FrametimeMs <= 17.0f) ? float4(0.0f, 1.0f, 0.6f, 1.0f) : float4(1.0f, 0.3f, 0.2f, 1.0f);
                    baseCol = lineColor;
                }
                else if (c.y > graphTop && c.y <= graphBaseY && relX > 15 && relX < hudWidth - 15)
                {
                    baseCol = lerp(baseCol, float4(0.0f, 0.8f, 0.5f, 1.0f), 0.25f);
                }

                // Top status indicator bar
                if (c.y >= hudTop + 8 && c.y <= hudTop + 14 && relX >= 15 && relX <= 25)
                {
                    baseCol = hudTheme;
                }

                g_SynthesizedFrame[coord] = baseCol;
            }
        }
    }
}
