package proxy

import (
	"bytes"
	"context"
	"fmt"
	"math"
	"net"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/sandertv/gophertunnel/minecraft"
	"github.com/sandertv/gophertunnel/minecraft/protocol"
	"github.com/sandertv/gophertunnel/minecraft/protocol/packet"
)

// TestUpstreamPackRequestsRespectWindow observes the production dialer's actual
// requests before any data, then its replenishment after out-of-order responses.
func TestUpstreamPackRequestsRespectWindow(t *testing.T) {
	window := minecraft.DefaultResourcePackMaxInFlightChunks
	for _, total := range []int{window - 1, window, window + 23} {
		t.Run(fmt.Sprintf("chunks_%d", total), func(t *testing.T) {
			id := uuid.New()
			packID := id.String() + "_1.0.0"
			network := newCacheStatusScriptedNetwork(func(conn net.Conn) error {
				decoder, encoder := packet.NewDecoder(conn), packet.NewEncoder(conn)
				if _, err := decoder.Decode(); err != nil {
					return fmt.Errorf("read network settings request: %w", err)
				}
				if err := encodeCacheStatusScriptedPackets(encoder, &packet.NetworkSettings{
					CompressionThreshold: math.MaxUint16,
					CompressionAlgorithm: packet.CompressionAlgorithmFlate,
				}); err != nil {
					return err
				}
				decoder.EnableCompression(packet.FlateCompression, math.MaxInt)
				encoder.EnableCompression(packet.FlateCompression, math.MaxUint16)
				if _, err := decoder.Decode(); err != nil {
					return fmt.Errorf("read login: %w", err)
				}
				if err := encodeCacheStatusScriptedPackets(encoder, &packet.PlayStatus{Status: packet.PlayStatusLoginSuccess}); err != nil {
					return err
				}
				if _, err := decoder.Decode(); err != nil {
					return fmt.Errorf("read cache status: %w", err)
				}
				if err := encodeCacheStatusScriptedPackets(encoder, &packet.ResourcePacksInfo{
					TexturePacks: []protocol.TexturePackInfo{{UUID: id, Version: "1.0.0", Size: uint64(total)}},
				}); err != nil {
					return err
				}
				batch, err := decoder.Decode()
				if err != nil {
					return fmt.Errorf("read pack selection: %w", err)
				}
				if len(batch) != 1 {
					return fmt.Errorf("pack selection count = %d, want 1", len(batch))
				}
				if err := encodeCacheStatusScriptedPackets(encoder, &packet.ResourcePackDataInfo{
					UUID: packID, DataChunkSize: 1, ChunkCount: uint32(total), Size: uint64(total),
				}); err != nil {
					return err
				}
				if err := expectPackRequestBatch(decoder, packID, 0, min(total, window)); err != nil {
					return fmt.Errorf("initial window: %w", err)
				}
				if total > window {
					for index, received := range []uint32{50, 0} {
						if err := encodeCacheStatusScriptedPackets(encoder, &packet.ResourcePackChunkData{
							UUID: packID, ChunkIndex: received, Data: []byte{byte(received)},
						}); err != nil {
							return err
						}
						if err := expectPackRequestBatch(decoder, packID, window+index, 1); err != nil {
							return fmt.Errorf("replenish after chunk %d: %w", received, err)
						}
					}
				}
				return nil
			})
			dialer := newUpstreamDialer(dialerTestDownstream{protocol: minecraft.DefaultProtocol}, nil)
			dialer.DownloadResourcePack = func(uuid.UUID, string, int, int) bool { return true }
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			conn, err := dialer.DialContextNetwork(ctx, network, "pack-window.invalid:19132")
			if conn != nil {
				_ = conn.Close()
			}
			if err == nil {
				t.Fatal("scripted server closed before full download but dial succeeded")
			}
			if scriptErr := <-network.done; scriptErr != nil {
				t.Fatal(scriptErr)
			}
		})
	}
}

// expectPackRequestBatch checks every request's identity, count and increasing index.
func expectPackRequestBatch(decoder *packet.Decoder, id string, first, count int) error {
	batch, err := decoder.Decode()
	if err != nil {
		return err
	}
	if len(batch) != count {
		return fmt.Errorf("request count = %d, want %d", len(batch), count)
	}
	for index, raw := range batch {
		buffer := bytes.NewBuffer(raw)
		var header packet.Header
		if err := header.Read(buffer); err != nil {
			return err
		}
		if header.PacketID != packet.IDResourcePackChunkRequest || header.SenderSubClient != 0 || header.TargetSubClient != 0 {
			return fmt.Errorf("unexpected pack request header at index %d", first+index)
		}
		request := new(packet.ResourcePackChunkRequest)
		request.Marshal(minecraft.DefaultProtocol.NewReader(buffer, 0, true))
		if buffer.Len() != 0 || request.UUID != id || request.ChunkIndex != int32(first+index) {
			return fmt.Errorf("unexpected pack request at index %d", first+index)
		}
	}
	return nil
}
