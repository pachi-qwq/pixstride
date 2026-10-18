struct VertexOutput
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

VertexOutput VSMain(uint vertex_id : SV_VertexID)
{
    const float2 positions[3] = {
        float2(-1.0, -1.0),
        float2(-1.0, 3.0),
        float2(3.0, -1.0),
    };
    const float2 texcoords[3] = {
        float2(0.0, 1.0),
        float2(0.0, -1.0),
        float2(2.0, 1.0),
    };

    VertexOutput output;
    output.position = float4(positions[vertex_id], 0.0, 1.0);
    output.uv = texcoords[vertex_id];
    return output;
}

Texture2D test_texture : register(t0);
SamplerState linear_sampler : register(s0);

float4 PSMain(VertexOutput input) : SV_Target
{
    return test_texture.Sample(linear_sampler, input.uv);
}
